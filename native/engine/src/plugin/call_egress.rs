//! 插件调用的网络出口：每次插件调用（resolve / 订阅 / 登录…）携带一个 [`CallEgress`]，
//! 其中所有 `flux.fetch` 与 `flux.ytdlp` 请求按它选出口。
//!
//! # 出口判定（与下载任务 `task_http_context` 同口径，匹配对象是调用的**源链接**）
//! 1. 任务 / 订阅自带代理（`proxy_url` 非空）→ 一律走它，不看站点规则；
//! 2. 源链接命中站点规则 → 未命中规则的请求沿用该规则出口（等同重定向沿用任务出口），
//!    其余请求仍逐条首条命中；
//! 3. 全局模式：None 直连 / Manual / System（调用时现场检测）/ Auto（[`plan_call`] 给出
//!    的路径计划）。
//!
//! # Auto 换路
//! 只有 Auto 计划会有多步。每步跑一次完整调用；失败且 [`NetObserver`] 证实是「走未命中
//! 规则出口的请求断在传输层」时换下一步重跑（见 `PluginManager::run_planned`）。判据来自
//! bridge 的类型化观测，不解析插件抛出的错误文本——插件可以吞掉、改写错误，直连被黑洞时
//! 还常以脚本墙钟超时收场。

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::auto_proxy::{self, CandidateSource, PathCandidate, RoutePath, route};
use crate::proxy_config::{ProxyConfig, ProxyMode};
use crate::proxy_rules::ProxyRuleSet;

use super::runtime::PluginError;

/// 一次插件调用的出口。由宿主注入（`HostContext.egress`），插件脚本不可见、不可改。
#[derive(Debug)]
pub struct CallEgress {
    /// 未命中站点规则的请求出口（已具体化：`None` 或 `Manual`，不带规则）。
    fallback: ProxyConfig,
    /// 出口来自显式设定：yt-dlp 必须注入 `--proxy`（直连注入空值强制直连）。
    /// 全局 None 模式与 Auto 的直连路径为 `false`，插件自带的 `--proxy` 仍可用。
    explicit: bool,
    /// 站点规则；任务 / 订阅自带代理时为 `None`。
    rules: Option<Arc<ProxyRuleSet>>,
    observer: NetObserver,
}

impl CallEgress {
    #[must_use]
    pub fn new(fallback: ProxyConfig, explicit: bool, rules: Option<Arc<ProxyRuleSet>>) -> Self {
        Self {
            fallback,
            explicit,
            rules,
            observer: NetObserver::default(),
        }
    }

    pub(crate) fn fallback(&self) -> &ProxyConfig {
        &self.fallback
    }

    pub(crate) fn explicit(&self) -> bool {
        self.explicit
    }

    pub(crate) fn rules(&self) -> Option<&Arc<ProxyRuleSet>> {
        self.rules.as_ref()
    }

    pub(crate) fn observer(&self) -> &NetObserver {
        &self.observer
    }
}

/// 一次调用内网络操作的观测（bridge 记录，manager 据此决定换路与熔断计数）。
#[derive(Debug, Default)]
pub struct NetObserver {
    /// 走未命中规则出口（fallback）的请求断在传输层的次数。
    fallback_failures: AtomicU32,
    /// 走 fallback 的请求在调用结束时仍未完成（被墙钟超时或取消丢弃）的次数。
    fallback_abandoned: AtomicU32,
    /// 任意出口的请求断在传输层的次数（熔断豁免判据）。
    failures: AtomicU32,
    /// 任意出口的请求未完成即被丢弃的次数（熔断豁免判据）。
    abandoned: AtomicU32,
}

impl NetObserver {
    /// 开始一次网络操作；返回的守卫在 [`NetOp::finish`] 前被丢弃即记为未完成。
    pub(crate) fn begin(egress: &Arc<CallEgress>, via_fallback: bool) -> NetOp {
        NetOp {
            egress: Arc::clone(egress),
            via_fallback,
            done: false,
        }
    }

    /// 调用失败是否可归因于 fallback 出口的传输层（换另一条出口重跑可能修复）。
    ///
    /// - `Runtime`：插件抛出的错误，要求本次调用确有 fallback 请求断在传输层；
    /// - `Timeout`（`retry_timeouts` 时）：fallback 请求断在传输层或超时时仍在等网络；
    /// - 其余（内存超限、过载、设置缺失、输出非法…）与出口无关。
    ///
    /// 有副作用的调用（登录：可能重发验证码、换新二维码）传 `retry_timeouts = false`：
    /// 超时时请求可能已经送达，重跑不安全。
    #[must_use]
    pub fn transport_suspect(&self, error: &PluginError, retry_timeouts: bool) -> bool {
        let failures = self.fallback_failures.load(Ordering::Acquire);
        match error {
            PluginError::Runtime(_) => failures > 0,
            PluginError::Timeout if retry_timeouts => {
                failures > 0 || self.fallback_abandoned.load(Ordering::Acquire) > 0
            }
            _ => false,
        }
    }

    /// 墙钟超时是否由网络造成（有请求断在传输层，或超时时仍在等网络）：被墙 / 慢网络
    /// 不是脚本缺陷，不计入熔断——否则一次 Auto 计划的每一步都会各计一次。
    #[must_use]
    pub fn network_caused_timeout(&self) -> bool {
        self.abandoned.load(Ordering::Acquire) > 0 || self.failures.load(Ordering::Acquire) > 0
    }
}

/// 进行中的网络操作。
pub(crate) struct NetOp {
    egress: Arc<CallEgress>,
    via_fallback: bool,
    done: bool,
}

impl NetOp {
    /// 操作结束；`transport_failed` = 断在传输层（建连 / DNS / TLS / 超时 / 中途断流）。
    pub(crate) fn finish(mut self, transport_failed: bool) {
        self.done = true;
        if !transport_failed {
            return;
        }
        let observer = &self.egress.observer;
        observer.failures.fetch_add(1, Ordering::AcqRel);
        if self.via_fallback {
            observer.fallback_failures.fetch_add(1, Ordering::AcqRel);
        }
    }
}

impl Drop for NetOp {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let observer = &self.egress.observer;
        observer.abandoned.fetch_add(1, Ordering::AcqRel);
        if self.via_fallback {
            observer.fallback_abandoned.fetch_add(1, Ordering::AcqRel);
        }
    }
}

/// 出口计划中的一步。
#[derive(Debug, Clone)]
pub struct EgressStep {
    /// Auto 路径；`None` = 非 Auto 的固定出口（自带代理 / 规则 / 全局模式）。
    pub route: Option<RoutePath>,
    /// 本步作为下载出口时的 `tasks.auto_route` 标签（仅 Auto 步有意义）。
    pub label: &'static str,
    /// 本步 fallback 出口（`None` / `Manual` 模式，不带规则）。
    pub proxy: ProxyConfig,
    /// 见 [`CallEgress`] 的 `explicit`。
    pub explicit: bool,
}

/// 一次调用的出口计划：按序尝试，只有 Auto 计划会多于一步。
#[derive(Debug, Clone, Default)]
pub struct EgressPlan {
    pub steps: Vec<EgressStep>,
    pub rules: Option<Arc<ProxyRuleSet>>,
    /// Auto 计划的 `route_health` 键（源链接 host）；非 Auto / 无候选 / 无 host 为 `None`。
    pub host: Option<String>,
}

impl EgressPlan {
    pub(crate) fn fixed(
        proxy: ProxyConfig,
        explicit: bool,
        rules: Option<Arc<ProxyRuleSet>>,
    ) -> Self {
        Self {
            steps: vec![EgressStep {
                route: None,
                label: "",
                proxy,
                explicit,
            }],
            rules,
            host: None,
        }
    }
}

/// 计划执行结果。
#[derive(Debug, Clone, Default)]
pub struct EgressOutcome {
    /// Auto 计划的 `route_health` 键。
    pub host: Option<String>,
    /// 断在传输层、已换路的 Auto 路径（按尝试顺序）。
    pub failed: Vec<RoutePath>,
    /// 产出成功结果的一步；调用失败为 `None`。
    pub used: Option<EgressStep>,
}

/// 任务 / 订阅侧的计划输入。
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanTask<'a> {
    /// 任务 / 订阅自带代理（`tasks.proxy_url` 语义，空串 = 跟随全局）。
    pub proxy_url: &'a str,
    /// 已排程的一次性备用链路（下载换路的目标）：作为首步。
    pub forced: Option<RoutePath>,
    /// 本轮自动恢复周期内已经走不通的路径：不再作为换路目标。
    pub tried: &'a [RoutePath],
}

/// 为一次以 `subject_url` 为源链接的插件调用制定出口计划。`global` 是带站点规则的全局
/// 代理配置。会阻塞（System 检测、Auto 系统候选检测），async 调用方须放进
/// `spawn_blocking`。
#[must_use]
pub fn plan_call(global: &ProxyConfig, subject_url: &str, task: PlanTask<'_>) -> EgressPlan {
    if !task.proxy_url.is_empty() {
        let proxy = ProxyConfig::from_proxy_url(task.proxy_url).resolve();
        return EgressPlan::fixed(proxy, true, None);
    }
    let rules = global.rules.clone();
    if let Some(rule) = rules.as_ref().and_then(|r| r.first_match(subject_url)) {
        let proxy = rule.egress().resolve();
        return EgressPlan::fixed(proxy, true, rules);
    }
    if global.mode != ProxyMode::Auto {
        let proxy = global.resolve();
        let explicit = proxy.mode != ProxyMode::None;
        return EgressPlan::fixed(proxy, explicit, rules);
    }
    let candidates = auto_proxy::resolve_candidates(global);
    // 无候选代理时计划只有直连一步，不记 route_health（换不了路，记了也无从受益）。
    let host =
        crate::segment_coordinator::extract_host(subject_url).filter(|_| !candidates.is_empty());
    let steps = plan_auto_steps(&candidates, host.as_deref(), task.forced, task.tried);
    EgressPlan { host, steps, rules }
}

/// Auto 计划（纯函数，`route_health` 只读）：
/// - 首步：已排程的备用链路，否则按跨任务先验起飞（`pick_auto_takeoff`）；
/// - 其余：本周期未试过、近期未断的代理按先验降序，直连最后。
fn plan_auto_steps(
    candidates: &[auto_proxy::ProxyCandidate],
    host: Option<&str>,
    forced: Option<RoutePath>,
    tried: &[RoutePath],
) -> Vec<EgressStep> {
    let prior = |route: RoutePath| {
        host.and_then(|h| crate::route_health::path_prior(h, route))
            .map(|p| p.bps)
    };
    let blocked =
        |route: RoutePath| host.is_some_and(|h| crate::route_health::path_blocked(h, route));
    let mut paths = Vec::with_capacity(candidates.len() + 1);
    paths.push(PathCandidate {
        route: RoutePath::Direct,
        config: ProxyConfig::default(),
        prior_bps: prior(RoutePath::Direct),
    });
    for candidate in candidates {
        let route = RoutePath::Proxy(candidate.source);
        paths.push(PathCandidate {
            route,
            config: candidate.config.clone(),
            prior_bps: prior(route),
        });
    }
    let blocked_flags: Vec<bool> = paths.iter().map(|p| blocked(p.route)).collect();
    plan_auto_steps_with(paths, &blocked_flags, forced, tried)
}

fn plan_auto_steps_with(
    mut paths: Vec<PathCandidate>,
    blocked: &[bool],
    forced: Option<RoutePath>,
    tried: &[RoutePath],
) -> Vec<EgressStep> {
    let step = |path: &PathCandidate, label: &'static str| EgressStep {
        route: Some(path.route),
        label,
        proxy: path.config.clone(),
        explicit: path.route != RoutePath::Direct,
    };
    let proxy_label = |base: &'static str, route: RoutePath| match route {
        RoutePath::Proxy(source) => route::with_source(base, source),
        RoutePath::Direct | RoutePath::Link(_) => route::DIRECT_FAILOVER,
    };
    let (first_index, first_label) = match forced {
        Some(target) => match paths.iter().position(|p| p.route == target) {
            Some(index) if index > 0 => (index, proxy_label(route::PROXY_FAILOVER, target)),
            // 直连目标或候选已不存在（代理设置变了）：同下载侧一样回落直连备用链路。
            _ => (0, route::DIRECT_FAILOVER),
        },
        None => {
            let index = crate::download_manager::pick_auto_takeoff(&paths, blocked);
            let label = match paths.get(index).map(|p| p.route) {
                Some(RoutePath::Proxy(source)) => route::with_source(route::PROXY_CACHED, source),
                _ => route::DIRECT,
            };
            (index, label)
        }
    };
    let mut flagged: Vec<(PathCandidate, bool)> = paths
        .drain(..)
        .zip(blocked.iter().copied().chain(std::iter::repeat(false)))
        .collect();
    let (first, _) = flagged.remove(first_index.min(flagged.len().saturating_sub(1)));
    let mut steps = vec![step(&first, first_label)];
    let mut rest: Vec<PathCandidate> = flagged
        .into_iter()
        .filter(|(path, is_blocked)| !is_blocked && !tried.contains(&path.route))
        .map(|(path, _)| path)
        .collect();
    // 代理在前（先验降序，同分保持候选顺序），直连最后。
    rest.sort_by(|a, b| {
        let rank = |p: &PathCandidate| matches!(p.route, RoutePath::Direct);
        rank(a).cmp(&rank(b)).then_with(|| {
            b.prior_bps
                .unwrap_or(0.0)
                .total_cmp(&a.prior_bps.unwrap_or(0.0))
        })
    });
    steps.extend(rest.iter().map(|path| {
        let label = match path.route {
            RoutePath::Proxy(_) => proxy_label(route::PROXY_FAILOVER, path.route),
            RoutePath::Direct | RoutePath::Link(_) => route::DIRECT_FAILOVER,
        };
        step(path, label)
    }));
    steps
}

/// yt-dlp 失败输出是否断在传输层。只看 `ERROR:` 行（`WARNING:` 行的重试提示不算最终
/// 结论）；HTTP 状态错误（403 / 404）、视频不可用、需要登录等不是传输失败。
#[must_use]
pub fn ytdlp_transport_failure(stderr: &str) -> bool {
    const MARKERS: [&str; 20] = [
        "transporterror",
        "urlopen error",
        "timed out",
        "read timeout",
        "connection reset",
        "connection refused",
        "connection aborted",
        "remote end closed connection",
        "remotedisconnected",
        "name or service not known",
        "nodename nor servname",
        "getaddrinfo failed",
        "temporary failure in name resolution",
        "no address associated with hostname",
        "failed to resolve",
        "network is unreachable",
        "no route to host",
        "unexpected_eof_while_reading",
        "eof occurred in violation of protocol",
        "unable to connect to proxy",
    ];
    stderr
        .lines()
        .filter(|line| line.contains("ERROR:"))
        .map(str::to_ascii_lowercase)
        .any(|line| MARKERS.iter().any(|marker| line.contains(marker)))
}

/// 来源标签（日志用）。
#[must_use]
pub fn route_name(route: Option<RoutePath>) -> &'static str {
    match route {
        Some(RoutePath::Direct) | Some(RoutePath::Link(_)) => "直连",
        Some(RoutePath::Proxy(CandidateSource::ManualFields)) => "手动代理",
        Some(RoutePath::Proxy(CandidateSource::System)) => "系统代理",
        None => "设定出口",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::proxy_config::ProxyType;

    const MANUAL: RoutePath = RoutePath::Proxy(CandidateSource::ManualFields);
    const SYSTEM: RoutePath = RoutePath::Proxy(CandidateSource::System);

    fn proxy(port: u16) -> ProxyConfig {
        ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Http,
            host: "127.0.0.1".to_string(),
            port,
            ..ProxyConfig::default()
        }
    }

    fn paths(priors: [Option<f64>; 3]) -> Vec<PathCandidate> {
        vec![
            PathCandidate {
                route: RoutePath::Direct,
                config: ProxyConfig::default(),
                prior_bps: priors[0],
            },
            PathCandidate {
                route: MANUAL,
                config: proxy(7890),
                prior_bps: priors[1],
            },
            PathCandidate {
                route: SYSTEM,
                config: proxy(7891),
                prior_bps: priors[2],
            },
        ]
    }

    fn routes(steps: &[EgressStep]) -> Vec<(RoutePath, &'static str)> {
        steps
            .iter()
            .map(|s| (s.route.expect("auto step"), s.label))
            .collect()
    }

    #[test]
    fn cold_plan_takes_off_direct_then_fails_over_to_each_proxy_once() {
        let steps = plan_auto_steps_with(paths([None; 3]), &[false; 3], None, &[]);
        assert_eq!(
            routes(&steps),
            vec![
                (RoutePath::Direct, route::DIRECT),
                (MANUAL, route::PROXY_FAILOVER_MANUAL),
                (SYSTEM, route::PROXY_FAILOVER_SYSTEM),
            ]
        );
        assert!(!steps[0].explicit, "Auto 直连不强制 yt-dlp 直连");
        assert!(steps[1].explicit);
        assert_eq!(steps[1].proxy.port, 7890);
    }

    #[test]
    fn blocked_direct_takes_off_on_proxy_and_is_not_retried() {
        let steps = plan_auto_steps_with(paths([None; 3]), &[true, false, false], None, &[]);
        assert_eq!(
            routes(&steps),
            vec![
                (MANUAL, route::PROXY_CACHED_MANUAL),
                (SYSTEM, route::PROXY_FAILOVER_SYSTEM),
            ]
        );
    }

    #[test]
    fn faster_proxy_is_retried_before_slower_and_direct_is_last() {
        let steps = plan_auto_steps_with(
            paths([Some(1e6), Some(1e5), Some(9e5)]),
            &[false; 3],
            None,
            &[],
        );
        assert_eq!(
            routes(&steps),
            vec![
                (RoutePath::Direct, route::DIRECT),
                (SYSTEM, route::PROXY_FAILOVER_SYSTEM),
                (MANUAL, route::PROXY_FAILOVER_MANUAL),
            ]
        );
        let proxy_first =
            plan_auto_steps_with(paths([None, Some(1e6), None]), &[false; 3], None, &[]);
        assert_eq!(
            routes(&proxy_first),
            vec![
                (MANUAL, route::PROXY_CACHED_MANUAL),
                (SYSTEM, route::PROXY_FAILOVER_SYSTEM),
                (RoutePath::Direct, route::DIRECT_FAILOVER),
            ]
        );
    }

    #[test]
    fn forced_target_leads_and_tried_paths_are_skipped() {
        let steps = plan_auto_steps_with(
            paths([None; 3]),
            &[false; 3],
            Some(SYSTEM),
            &[RoutePath::Direct],
        );
        assert_eq!(
            routes(&steps),
            vec![
                (SYSTEM, route::PROXY_FAILOVER_SYSTEM),
                (MANUAL, route::PROXY_FAILOVER_MANUAL),
            ]
        );
        let direct = plan_auto_steps_with(
            paths([None; 3]),
            &[false; 3],
            Some(RoutePath::Direct),
            &[MANUAL, SYSTEM],
        );
        assert_eq!(
            routes(&direct),
            vec![(RoutePath::Direct, route::DIRECT_FAILOVER)]
        );
    }

    #[test]
    fn forced_proxy_that_no_longer_exists_falls_back_to_direct_failover() {
        let only_direct = vec![paths([None; 3]).remove(0)];
        let steps = plan_auto_steps_with(only_direct, &[false], Some(MANUAL), &[]);
        assert_eq!(
            routes(&steps),
            vec![(RoutePath::Direct, route::DIRECT_FAILOVER)]
        );
    }

    #[test]
    fn fixed_plans_follow_task_proxy_then_rules_then_global_mode() {
        let rules = Arc::new(ProxyRuleSet::compile(
            r#"[{"id":"a","match":"domain","pattern":"video.test","action":"custom","proxy":"http://127.0.0.1:9000"}]"#,
            &ProxyConfig::default(),
        ));
        let global = ProxyConfig {
            mode: ProxyMode::None,
            rules: Some(rules),
            ..ProxyConfig::default()
        };
        let task = plan_call(
            &global,
            "https://www.video.test/watch",
            PlanTask {
                proxy_url: "socks5://127.0.0.1:1080",
                ..PlanTask::default()
            },
        );
        assert_eq!(task.steps.len(), 1);
        assert_eq!(task.steps[0].proxy.port, 1080);
        assert!(task.rules.is_none(), "自带代理不看规则");

        let rule = plan_call(&global, "https://www.video.test/watch", PlanTask::default());
        assert_eq!(rule.steps[0].proxy.port, 9000);
        assert!(rule.steps[0].explicit);
        assert!(rule.rules.is_some(), "其它请求仍逐条匹配规则");

        let none = plan_call(&global, "https://other.test/", PlanTask::default());
        assert_eq!(none.steps[0].proxy.mode, ProxyMode::None);
        assert!(!none.steps[0].explicit, "未配置代理时不强制 yt-dlp 直连");

        let direct = plan_call(
            &global,
            "https://other.test/",
            PlanTask {
                proxy_url: ProxyConfig::DIRECT_SENTINEL,
                ..PlanTask::default()
            },
        );
        assert!(direct.steps[0].explicit, "任务显式直连强制 yt-dlp 直连");
    }

    #[test]
    fn observer_only_blames_fallback_transport_failures() {
        let egress = Arc::new(CallEgress::new(ProxyConfig::default(), false, None));
        let observer = || egress.observer();
        let runtime = PluginError::Runtime("video unavailable".to_string());
        assert!(!observer().transport_suspect(&runtime, true));
        assert!(!observer().transport_suspect(&PluginError::Timeout, true));

        NetObserver::begin(&egress, false).finish(true);
        assert!(
            !observer().transport_suspect(&runtime, true),
            "规则出口的失败换 fallback 修不好"
        );
        assert!(
            observer().network_caused_timeout(),
            "任意出口断网都不是脚本缺陷"
        );
        NetObserver::begin(&egress, true).finish(false);
        assert!(!observer().transport_suspect(&runtime, true));

        NetObserver::begin(&egress, true).finish(true);
        assert!(observer().transport_suspect(&runtime, true));
        assert!(observer().transport_suspect(&runtime, false));
        assert!(!observer().transport_suspect(&PluginError::MemoryLimitExceeded, true));
    }

    #[test]
    fn abandoned_fallback_request_marks_timeout_as_network() {
        let egress = Arc::new(CallEgress::new(ProxyConfig::default(), false, None));
        let observer = || egress.observer();
        assert!(!observer().network_caused_timeout());
        drop(NetObserver::begin(&egress, false));
        assert!(observer().network_caused_timeout());
        assert!(
            !observer().transport_suspect(&PluginError::Timeout, true),
            "等的是规则出口"
        );
        drop(NetObserver::begin(&egress, true));
        assert!(observer().transport_suspect(&PluginError::Timeout, true));
        assert!(
            !observer().transport_suspect(&PluginError::Timeout, false),
            "有副作用的调用超时不重跑"
        );
        assert!(
            !observer().transport_suspect(&PluginError::Runtime("x".to_string()), true),
            "抛错时只认已完成的传输失败"
        );
    }

    #[test]
    fn ytdlp_transport_markers_only_count_on_error_lines() {
        assert!(ytdlp_transport_failure(
            "ERROR: [youtube] abc: Unable to download API page: <urlopen error [Errno 60] Operation timed out> (caused by URLError(TimeoutError(60, 'Operation timed out')))"
        ));
        assert!(ytdlp_transport_failure(
            "ERROR: [youtube] abc: Unable to download webpage: (caused by TransportError('[Errno 8] nodename nor servname provided, or not known'))"
        ));
        assert!(ytdlp_transport_failure(
            "ERROR: Unable to download webpage: [Errno 104] Connection reset by peer"
        ));
        assert!(!ytdlp_transport_failure(
            "ERROR: [youtube] abc: Video unavailable. This video is private"
        ));
        assert!(!ytdlp_transport_failure(
            "ERROR: [youtube] abc: Sign in to confirm you’re not a bot"
        ));
        assert!(!ytdlp_transport_failure(
            "ERROR: unable to download video data: HTTP Error 403: Forbidden"
        ));
        assert!(!ytdlp_transport_failure(
            "WARNING: [youtube] Unable to download webpage: <urlopen error timed out>; retrying\nERROR: [youtube] abc: Private video"
        ));
    }
}
