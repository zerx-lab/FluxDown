//! 任务起跑时按系统 PAC 修正出口。在 spawned task 内运行（不阻塞 actor），
//! start 与 resume 共用（`run_http_family_task`），覆盖 HTTP / FTP / HLS / DASH。
//!
//! - **System 模式**：只在「系统没有启用静态代理、配置了 PAC（含 WPAD）、且本机
//!   能执行 PAC 脚本（`plugins` feature）」时生效，按 `FindProxyForURL` 对本任务
//!   链接的结论选出口；列表中经 PAC 选中的代理传输失败后 5 分钟内排到最后
//!   （Chromium 语义），下一次自动重试自然换到下一条。其余情况行为与历史一致
//!   （静态代理照用；PAC 无法执行时直连）。
//! - **Auto 模式**：PAC 对本链接的结论决定「系统代理路径」指向哪个端点——结论是
//!   `DIRECT` 时系统代理路径与直连重合，从候选里去掉；结论是某个代理时用它（与
//!   手动代理同端点则去重）；actor 决策时 PAC 脚本尚未缓存而缺席的系统代理路径
//!   在这里补上。一次性备用链路（单路径上下文）不改。

use std::sync::Arc;

use crate::auto_proxy::{AutoProxyCtx, CandidateSource, PathCandidate, RoutePath, route};
use crate::downloader::DownloadParams;
use crate::events::EngineEvent;
use crate::logger::log_info;
use crate::pac::PacEntry;
use crate::proxy_config::{ProxyConfig, ProxyMode};

/// 本任务适用的 PAC 路由（manager 按全局代理模式与任务级代理折算）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PacRouting {
    /// 任务指定了代理，或全局不是 System / Auto：不参与。
    Off,
    System,
    Auto,
}

const SYSTEM_ROUTE: RoutePath = RoutePath::Proxy(CandidateSource::System);

/// 按 PAC 修正 `params` 的出口。返回 System 模式下经 PAC 选中的代理（任务以
/// 传输失败告终时由调用方 [`report_outcome`] 标坏）；其余情况 `None`。
pub(crate) async fn apply(params: &mut DownloadParams, routing: PacRouting) -> Option<ProxyConfig> {
    if routing == PacRouting::Off {
        return None;
    }
    // 读系统配置可能起子进程（macOS scutil / GNOME gsettings），放到阻塞线程池。
    let pac_url = match tokio::task::spawn_blocking(crate::proxy_config::system_pac_url).await {
        Ok(found) => found?,
        Err(error) => {
            log_info!(
                "[pac] task {} 读取系统 PAC 配置失败: {error}",
                params.task_id
            );
            return None;
        }
    };
    apply_with_pac_url(params, routing, &pac_url).await
}

/// [`apply`] 的 PAC 地址注入变体（测试确定性；生产路径恒经 [`apply`]）。
async fn apply_with_pac_url(
    params: &mut DownloadParams,
    routing: PacRouting,
    pac_url: &str,
) -> Option<ProxyConfig> {
    match routing {
        PacRouting::System => apply_system(params, pac_url).await,
        PacRouting::Auto => {
            refine_auto(params, pac_url).await;
            None
        }
        PacRouting::Off => None,
    }
}

async fn apply_system(params: &mut DownloadParams, pac_url: &str) -> Option<ProxyConfig> {
    // 静态系统代理已启用：沿用（历史行为，PAC 与静态并存时静态优先）。
    if params.proxy_config.mode != ProxyMode::None {
        return None;
    }
    let decision = crate::pac::decide(pac_url, &params.url).await?;
    if !decision.evaluated {
        // 没能真正执行脚本：不知道本链接该不该走代理，保持历史行为（直连）。
        return None;
    }
    let chosen = match crate::pac::pick(&decision.entries)? {
        PacEntry::Direct => {
            log_info!("[pac] task {} PAC 结论 DIRECT", params.task_id);
            return None;
        }
        PacEntry::Proxy(cfg) => cfg.clone(),
    };
    let client = match crate::downloader::build_client_with_tls_policy(
        &chosen,
        &params.run_meta.user_agent,
        params.run_meta.ignore_tls_errors,
    ) {
        Ok(client) => client,
        Err(error) => {
            log_info!(
                "[pac] task {} 构建 PAC 代理 client 失败，保持直连: {error:#}",
                params.task_id
            );
            return None;
        }
    };
    log_info!(
        "[pac] task {} PAC 结论 {}://{}:{}",
        params.task_id,
        chosen.proxy_type.scheme(),
        chosen.host,
        chosen.port
    );
    params.client = client;
    params.proxy_config = chosen.clone();
    route_through_proxy(params);
    // 起跑时按静态系统代理（未检测到 → `global:direct`）落了标签；PAC 选中代理后改正。
    if let Err(error) = params
        .db
        .set_task_auto_route(&params.task_id, route::GLOBAL_PAC)
        .await
    {
        log_info!(
            "[pac] task {} auto_route 落库失败: {error:#}",
            params.task_id
        );
    }
    params.sink.emit(EngineEvent::TaskRouteChanged {
        task_id: params.task_id.clone(),
        route: route::GLOBAL_PAC.to_owned(),
    });
    Some(chosen)
}

/// 出口改走代理后，多 CDN 钉定节点（直连 IP）与多网卡聚合（绑定本机网卡直连）
/// 都会旁路代理，必须关闭。
pub(crate) fn route_through_proxy(params: &mut DownloadParams) {
    params.cdn.enabled = false;
    if let Some(input) = params.multi_nic.as_ref().filter(|i| !i.blocked_by_proxy) {
        let mut blocked = (**input).clone();
        blocked.blocked_by_proxy = true;
        params.multi_nic = Some(Arc::new(blocked));
    }
}

/// System 模式任务结束：以传输层失败告终时把经 PAC 选中的代理标坏 5 分钟。
pub(crate) async fn report_outcome(db: &crate::db::Db, task_id: &str, used: &ProxyConfig) {
    match db.load_task_by_id(task_id).await {
        Ok(Some(task))
            if task.status == 4
                && crate::auto_proxy::is_route_transport_error(&task.error_message) =>
        {
            log_info!(
                "[pac] task {task_id} 经 PAC 代理 {}:{} 传输失败，5 分钟内排到列表最后",
                used.host,
                used.port
            );
            crate::pac::mark_bad(used);
        }
        Ok(_) => {}
        Err(error) => log_info!("[pac] task {task_id} 读取任务结果失败: {error}"),
    }
}

/// 本链接上「系统代理路径」应指向的端点；`None` = 按 PAC 该链接直连（系统代理
/// 路径与直连重合）或 PAC 里没有代理。
fn system_endpoint(decision: &crate::pac::PacDecision) -> Option<ProxyConfig> {
    if decision.evaluated {
        match crate::pac::pick(&decision.entries)? {
            PacEntry::Proxy(cfg) => Some(cfg.clone()),
            PacEntry::Direct => None,
        }
    } else {
        decision.entries.iter().find_map(|entry| match entry {
            PacEntry::Proxy(cfg) => Some(cfg.clone()),
            PacEntry::Direct => None,
        })
    }
}

async fn refine_auto(params: &mut DownloadParams, pac_url: &str) {
    let Some(host) = crate::segment_coordinator::extract_host(&params.url) else {
        return;
    };
    let Some(decision) = crate::pac::decide(pac_url, &params.url).await else {
        return;
    };
    let system = system_endpoint(&decision);
    let ctx = match params.auto_proxy.clone() {
        // 一次性备用链路：只走指定路径，不改。
        Some(ctx) if ctx.alternates.is_empty() => return,
        Some(ctx) => ctx,
        None => {
            // actor 决策时没有任何候选（PAC 脚本尚未缓存）：直连起飞 + PAC 系统代理备选。
            let eligible = params.proxy_config.mode == ProxyMode::None
                && !params.run_meta.ignore_tls_errors
                && !crate::route_health::no_switch_active(&host);
            let Some(cfg) = system.clone().filter(|_| eligible) else {
                return;
            };
            Arc::new(AutoProxyCtx {
                start_prior_bps: crate::route_health::path_prior(&host, RoutePath::Direct)
                    .map(|p| p.bps),
                host: host.clone(),
                user_agent: params.run_meta.user_agent.clone(),
                start_route: RoutePath::Direct,
                start_label: route::DIRECT,
                alternates: vec![PathCandidate {
                    route: SYSTEM_ROUTE,
                    prior_bps: crate::route_health::path_prior(&host, SYSTEM_ROUTE).map(|p| p.bps),
                    config: cfg,
                }],
            })
        }
    };
    let start = PathCandidate {
        route: ctx.start_route,
        config: params.proxy_config.clone(),
        prior_bps: ctx.start_prior_bps,
    };
    let system_prior = crate::route_health::path_prior(&host, SYSTEM_ROUTE).map(|p| p.bps);
    let paths = rewrite_paths(start, &ctx.alternates, system, system_prior);
    let Some((new_start, alternates)) = paths.split_first() else {
        return;
    };
    let start_moved = new_start.route != ctx.start_route
        || new_start.config.to_proxy_url() != params.proxy_config.to_proxy_url();
    if start_moved {
        match crate::downloader::build_client_with_tls_policy(
            &new_start.config,
            &ctx.user_agent,
            false,
        ) {
            Ok(client) => params.client = client,
            Err(error) => {
                log_info!(
                    "[pac] task {} 构建起飞路径 client 失败，保持原出口: {error:#}",
                    params.task_id
                );
                return;
            }
        }
        params.proxy_config = new_start.config.clone();
    }
    let label = if new_start.route == ctx.start_route {
        ctx.start_label
    } else {
        match new_start.route {
            RoutePath::Direct | RoutePath::Link(_) => route::DIRECT,
            RoutePath::Proxy(source) => route::with_source(route::PROXY_CACHED, source),
        }
    };
    log_info!(
        "[pac] task {} host {} PAC 修正 Auto 路径：起飞 {:?}，备选 {:?}",
        params.task_id,
        host,
        new_start.route,
        alternates.iter().map(|a| a.route).collect::<Vec<_>>()
    );
    route_through_proxy_if_needed(params, new_start.route, !alternates.is_empty());
    params.auto_proxy = (!alternates.is_empty()).then(|| {
        Arc::new(AutoProxyCtx {
            host: ctx.host.clone(),
            user_agent: ctx.user_agent.clone(),
            start_route: new_start.route,
            start_prior_bps: new_start.prior_bps,
            start_label: label,
            alternates: alternates.to_vec(),
        })
    });
    if label != ctx.start_label {
        if let Err(error) = params.db.set_task_auto_route(&params.task_id, label).await {
            log_info!(
                "[pac] task {} auto_route 落库失败: {error:#}",
                params.task_id
            );
        }
        params.sink.emit(EngineEvent::TaskRouteChanged {
            task_id: params.task_id.clone(),
            route: label.to_string(),
        });
    }
}

/// Auto 起飞走代理时关多 CDN；存在备选路径（多路径）时多网卡聚合同样让位——
/// 与 manager 对 Auto 多路径任务的折算一致。
fn route_through_proxy_if_needed(params: &mut DownloadParams, start: RoutePath, multipath: bool) {
    if start != RoutePath::Direct {
        params.cdn.enabled = false;
    }
    if (start != RoutePath::Direct || multipath)
        && let Some(input) = params.multi_nic.as_ref().filter(|i| !i.blocked_by_proxy)
    {
        let mut blocked = (**input).clone();
        blocked.blocked_by_proxy = true;
        params.multi_nic = Some(Arc::new(blocked));
    }
}

/// 纯函数：按本链接的 PAC 结论改写 Auto 路径表（`[起飞, 备选…]`）。
///
/// - `system = None`（PAC 结论直连）：去掉系统代理路径；若它是起飞路径，由直连接替起飞；
/// - `system = Some(端点)`：系统代理路径改指该端点，缺席则以 `system_prior` 补为备选；
/// - 同一代理端点只保留先出现的路径（手动代理在前）。
fn rewrite_paths(
    start: PathCandidate,
    alternates: &[PathCandidate],
    system: Option<ProxyConfig>,
    system_prior: Option<f64>,
) -> Vec<PathCandidate> {
    let mut paths: Vec<PathCandidate> = std::iter::once(start)
        .chain(alternates.iter().cloned())
        .collect();
    let system_index = paths.iter().position(|p| p.route == SYSTEM_ROUTE);
    match (system, system_index) {
        (None, Some(index)) => {
            paths.remove(index);
            if index == 0
                && let Some(direct) = paths.iter().position(|p| p.route == RoutePath::Direct)
            {
                let direct = paths.remove(direct);
                paths.insert(0, direct);
            }
        }
        (Some(cfg), Some(index)) => {
            if let Some(path) = paths.get_mut(index) {
                path.config = cfg;
            }
        }
        (Some(cfg), None) => paths.push(PathCandidate {
            route: SYSTEM_ROUTE,
            config: cfg,
            prior_bps: system_prior,
        }),
        (None, None) => {}
    }
    let mut seen: Vec<String> = Vec::new();
    paths.retain(|path| match path.config.to_proxy_url() {
        Some(url) if seen.contains(&url) => false,
        Some(url) => {
            seen.push(url);
            true
        }
        None => true,
    });
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy_config::ProxyType;

    const MANUAL: RoutePath = RoutePath::Proxy(CandidateSource::ManualFields);

    fn proxy(port: u16) -> ProxyConfig {
        ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Http,
            host: "127.0.0.1".to_string(),
            port,
            ..ProxyConfig::default()
        }
    }

    fn path(route: RoutePath, config: ProxyConfig) -> PathCandidate {
        PathCandidate {
            route,
            config,
            prior_bps: None,
        }
    }

    fn shape(paths: &[PathCandidate]) -> Vec<(RoutePath, u16)> {
        paths.iter().map(|p| (p.route, p.config.port)).collect()
    }

    #[test]
    fn pac_direct_removes_system_path_and_direct_takes_over_takeoff() {
        let start = path(SYSTEM_ROUTE, proxy(7890));
        let alternates = [
            path(RoutePath::Direct, ProxyConfig::default()),
            path(MANUAL, proxy(1080)),
        ];
        let paths = rewrite_paths(start, &alternates, None, None);
        assert_eq!(shape(&paths), vec![(RoutePath::Direct, 0), (MANUAL, 1080)]);
    }

    #[test]
    fn pac_proxy_retargets_or_adds_system_path_and_dedups_manual() {
        let start = path(RoutePath::Direct, ProxyConfig::default());
        // 系统代理路径改指 PAC 给出的端点。
        let paths = rewrite_paths(
            start.clone(),
            &[path(SYSTEM_ROUTE, proxy(7890))],
            Some(proxy(7891)),
            None,
        );
        assert_eq!(
            shape(&paths),
            vec![(RoutePath::Direct, 0), (SYSTEM_ROUTE, 7891)]
        );
        // 缺席时补为备选，带先验。
        let paths = rewrite_paths(start.clone(), &[], Some(proxy(7891)), Some(2e6));
        assert_eq!(
            shape(&paths),
            vec![(RoutePath::Direct, 0), (SYSTEM_ROUTE, 7891)]
        );
        assert_eq!(paths[1].prior_bps, Some(2e6));
        // 与手动代理同端点：只保留手动（先出现者）。
        let paths = rewrite_paths(start, &[path(MANUAL, proxy(7891))], Some(proxy(7891)), None);
        assert_eq!(shape(&paths), vec![(RoutePath::Direct, 0), (MANUAL, 7891)]);
    }

    #[test]
    fn pac_direct_without_system_path_changes_nothing() {
        let start = path(RoutePath::Direct, ProxyConfig::default());
        let paths = rewrite_paths(start, &[path(MANUAL, proxy(1080))], None, None);
        assert_eq!(shape(&paths), vec![(RoutePath::Direct, 0), (MANUAL, 1080)]);
    }

    /// 把 PAC 脚本写进临时文件，返回 `file://` 地址（每个测试独立文件，缓存互不干扰）。
    #[cfg(feature = "plugins")]
    fn pac_file(name: &str, script: &str) -> Result<String, Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("fluxdown-pac-route-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(name);
        std::fs::write(&path, script)?;
        Ok(url::Url::from_file_path(&path)
            .map_err(|()| "file url")?
            .to_string())
    }

    #[cfg(feature = "plugins")]
    async fn params_for(url: &str) -> Result<DownloadParams, Box<dyn std::error::Error>> {
        crate::downloader::test_support::params_with_interceptor(url, crate::intercept::noop())
            .await
    }

    /// System 模式：PAC 按链接分流；静态代理优先；PAC 选中的代理失败后排到最后。
    #[cfg(feature = "plugins")]
    #[tokio::test]
    async fn system_mode_follows_pac_per_url_and_demotes_failed_proxy()
    -> Result<(), Box<dyn std::error::Error>> {
        let pac = pac_file(
            "system.pac",
            r#"function FindProxyForURL(url, host) {
                if (dnsDomainIs(host, ".blocked.test")) return "PROXY 127.0.0.1:18080; DIRECT";
                return "DIRECT";
            }"#,
        )?;

        let mut blocked = params_for("http://dl.blocked.test/f.bin").await?;
        blocked.cdn.enabled = true;
        let used = apply_with_pac_url(&mut blocked, PacRouting::System, &pac).await;
        assert_eq!(used.as_ref().map(|c| c.port), Some(18080));
        assert_eq!(blocked.proxy_config.port, 18080, "出口改走 PAC 代理");
        assert!(!blocked.cdn.enabled, "走代理后不得再用直连 IP 的多 CDN");

        let mut domestic = params_for("http://cdn.example.cn/f.bin").await?;
        assert!(
            apply_with_pac_url(&mut domestic, PacRouting::System, &pac)
                .await
                .is_none()
        );
        assert_eq!(
            domestic.proxy_config.mode,
            ProxyMode::None,
            "PAC 说直连就直连"
        );

        // 系统已启用静态代理：保持历史行为，不看 PAC。
        let mut static_proxy = params_for("http://dl.blocked.test/f.bin").await?;
        static_proxy.proxy_config = proxy(3128);
        assert!(
            apply_with_pac_url(&mut static_proxy, PacRouting::System, &pac)
                .await
                .is_none()
        );
        assert_eq!(static_proxy.proxy_config.port, 3128);

        // PAC 代理传输失败后 5 分钟内排到最后：下一次运行按列表走 DIRECT。
        crate::pac::mark_bad(&proxy(18080));
        let mut retry = params_for("http://dl.blocked.test/f.bin").await?;
        assert!(
            apply_with_pac_url(&mut retry, PacRouting::System, &pac)
                .await
                .is_none()
        );
        assert_eq!(retry.proxy_config.mode, ProxyMode::None);
        Ok(())
    }

    /// Auto 模式：PAC 对本链接说直连 → 系统代理路径与直连重合、去掉；说某代理 →
    /// 系统代理路径改指该端点；actor 决策时缺席的系统代理路径补上。
    #[cfg(feature = "plugins")]
    #[tokio::test]
    async fn auto_mode_retargets_system_path_per_url() -> Result<(), Box<dyn std::error::Error>> {
        let pac = pac_file(
            "auto.pac",
            r#"function FindProxyForURL(url, host) {
                if (shExpMatch(host, "*.blocked.test")) return "SOCKS5 127.0.0.1:17891";
                return "DIRECT";
            }"#,
        )?;
        let ctx = |host: &str| {
            Arc::new(AutoProxyCtx {
                host: host.to_string(),
                user_agent: String::new(),
                start_route: RoutePath::Direct,
                start_prior_bps: None,
                start_label: route::DIRECT,
                alternates: vec![path(SYSTEM_ROUTE, proxy(17890))],
            })
        };

        let mut domestic = params_for("https://cdn.example.cn/f.bin").await?;
        domestic.auto_proxy = Some(ctx("cdn.example.cn"));
        apply_with_pac_url(&mut domestic, PacRouting::Auto, &pac).await;
        assert!(
            domestic.auto_proxy.is_none(),
            "系统代理路径与直连重合，不再竞速"
        );
        assert_eq!(domestic.proxy_config.mode, ProxyMode::None);

        let mut blocked = params_for("https://dl.blocked.test/f.bin").await?;
        blocked.auto_proxy = Some(ctx("dl.blocked.test"));
        apply_with_pac_url(&mut blocked, PacRouting::Auto, &pac).await;
        let alternates = &blocked.auto_proxy.as_ref().ok_or("ctx")?.alternates;
        assert_eq!(alternates.len(), 1);
        assert_eq!(alternates[0].route, SYSTEM_ROUTE);
        assert_eq!(
            alternates[0].config.port, 17891,
            "改指 PAC 对本链接给出的端点"
        );
        assert_eq!(alternates[0].config.proxy_type, ProxyType::Socks5);

        // actor 决策时 PAC 尚未缓存（无候选、无上下文）：补出直连起飞 + 系统代理备选。
        let mut cold = params_for("https://dl.blocked.test/g.bin").await?;
        apply_with_pac_url(&mut cold, PacRouting::Auto, &pac).await;
        let ctx = cold.auto_proxy.as_ref().ok_or("ctx created")?;
        assert_eq!(ctx.start_route, RoutePath::Direct);
        assert_eq!(ctx.alternates.first().map(|a| a.config.port), Some(17891));
        Ok(())
    }
}
