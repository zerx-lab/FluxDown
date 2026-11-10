//! `ProxyMode::Auto` 探测阶段的多路径竞速（Happy Eyeballs 式）。
//!
//! # 为什么需要
//!
//! 多路径调度（[`crate::path_scheduler`]）只在多段下载起飞**之后**生效；起飞
//! 前的探测只走起飞路径。直连被墙（RST / 黑洞 / DNS 污染）时探测要跑满
//! 3 轮重试（黑洞约 63s）才失败，任务根本到不了多路径阶段；不支持 Range 的
//! 单流下载更是全程只有起飞路径。
//!
//! # 算法（RFC 8305 §5 的「路径」版）
//!
//! 1. 起飞路径先发探测；
//! 2. 起飞路径在错峰延迟（stagger）内无结果，或任一轮断在传输层（类型化
//!    分类，[`crate::net_failure`]）→ 立即并行放出全部备选路径；
//! 3. 第一条**成功**的路径胜出，其余在途探测随 future drop 取消；HTTP 层
//!    失败（403 等）不终止竞速——别的路径仍可能成功（地域封锁）；
//! 4. 起飞路径在错峰前就拿到 HTTP 层失败：服务器已应答、链路可达，不竞速；
//! 5. 胜者不是起飞路径 → 改写任务 client / 代理配置 / 多路径上下文并发布
//!    路由标签；断在传输层的路径记入 [`crate::route_health`]（下次起飞避开）。
//!
//! 错峰延迟：起飞路径近期在本 host 被实测验证过（有跨任务先验）→ 2s（RFC
//! 8305 建议上限，给已知可用路径充分机会）；无先验 → 1s；已知走不通 → 0。
//! 只在 GET 类请求上竞速：POST 等有副作用的请求不得经两条路径重放。

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use reqwest::Client;

use crate::auto_proxy::{AutoProxyCtx, PathCandidate, RoutePath, route};
use crate::downloader::{
    DownloadError, DownloadParams, FileInfo, PreprobedInfo, ProbeKind, RequestSpec,
    adapt_probe_request,
};
use crate::events::EngineEvent;
use crate::logger::log_info;
use crate::proxy_config::ProxyConfig;

/// 起飞路径近期被验证过时的错峰延迟。
const STAGGER_PROVEN: Duration = Duration::from_secs(2);
/// 起飞路径无先验时的错峰延迟。
const STAGGER_COLD: Duration = Duration::from_secs(1);

/// 单条路径探测的产出。
type ProbeOutput = (FileInfo, Option<RequestSpec>, Option<reqwest::Response>);

/// 竞速中一条腿的结果：`(腿序号, 该腿 client, 探测结果)`；序号 0 = 起飞路径，
/// `i + 1` = `ctx.alternates[i]`。
type LegResult = (usize, Client, Result<ProbeOutput, DownloadError>);

type LegFuture = Pin<Box<dyn Future<Output = LegResult> + Send>>;

/// 本任务的探测是否走竞速：Auto 多路径上下文存在且有备选路径，且请求可安全
/// 经多条路径重放（GET 类）。
pub(crate) fn applies(params: &DownloadParams) -> bool {
    params.spec.is_get_like()
        && params
            .auto_proxy
            .as_ref()
            .is_some_and(|ctx| !ctx.alternates.is_empty())
}

/// 竞速探测。调用方先以 [`applies`] 判定；不适用时退化为起飞路径单独探测。
/// 成功时 `params` 的 client / 代理配置 / 多路径上下文已指向胜出路径。
pub(crate) async fn race_probe(
    params: &mut DownloadParams,
    kind: ProbeKind,
) -> Result<PreprobedInfo, DownloadError> {
    let Some(ctx) = params
        .auto_proxy
        .clone()
        .filter(|ctx| !ctx.alternates.is_empty())
    else {
        let (info, adapted, held) =
            adapt_probe_request(&params.client, &params.url, &params.spec, kind, None).await?;
        return Ok(PreprobedInfo {
            info,
            adapted,
            held,
        });
    };
    let outcome = run_race(params, &ctx, kind).await;
    if outcome.cancelled {
        return Err(DownloadError::Cancelled);
    }
    // 全部路径都失败时，任务以起飞路径的错误终止，manager 的 failover 支路
    // 会按任务路由标签把起飞路径记一次失败——这里只记备选路径，避免重复计权。
    let recorded: Vec<usize> = if outcome.result.is_ok() {
        outcome.failed.clone()
    } else {
        outcome.failed.iter().copied().filter(|&i| i != 0).collect()
    };
    record_failures(&ctx, &recorded, &params.db);
    let (winner, client, (info, adapted, held)) = outcome.result?;
    if winner != 0 {
        adopt_alternate(params, &ctx, winner, client, &outcome.failed).await;
    }
    Ok(PreprobedInfo {
        info,
        adapted,
        held,
    })
}

struct RaceOutcome {
    /// 胜出的 `(腿序号, client, 结果)`，或全部失败时代表性的错误。
    result: Result<(usize, Client, ProbeOutput), DownloadError>,
    /// 断在传输层的腿序号。
    failed: Vec<usize>,
    cancelled: bool,
}

/// 起飞路径探测的进展信号：探测阶梯（[`adapt_probe_request`]）每轮失败后
/// 上报一次。断在传输层 → 竞速立即放出备选路径；拿到服务器应答（HTTP 层
/// 失败，阶梯会换请求头重试）→ 链路可达，撤销错峰计时，不再因「慢」竞速。
#[derive(Default)]
pub(crate) struct PathSignal {
    notify: tokio::sync::Notify,
    answered: AtomicBool,
    transport_failed: AtomicBool,
}

impl PathSignal {
    /// 记录一轮探测失败的类别并唤醒竞速。
    pub(crate) fn observe(&self, error: &DownloadError) {
        if crate::net_failure::classify_download_error(error).is_some() {
            self.transport_failed.store(true, Ordering::Release);
        } else {
            self.answered.store(true, Ordering::Release);
        }
        self.notify.notify_one();
    }
}

/// 错峰延迟（见模块文档）。
fn stagger_for(ctx: &AutoProxyCtx) -> Duration {
    if crate::route_health::path_blocked(&ctx.host, ctx.start_route) {
        Duration::ZERO
    } else if ctx.start_prior_bps.is_some() {
        STAGGER_PROVEN
    } else {
        STAGGER_COLD
    }
}

fn leg(
    index: usize,
    client: Client,
    url: String,
    spec: RequestSpec,
    kind: ProbeKind,
    signal: Option<Arc<PathSignal>>,
) -> LegFuture {
    Box::pin(async move {
        let result = adapt_probe_request(&client, &url, &spec, kind, signal.as_deref()).await;
        (index, client, result)
    })
}

async fn run_race(params: &DownloadParams, ctx: &AutoProxyCtx, kind: ProbeKind) -> RaceOutcome {
    let start_signal = Arc::new(PathSignal::default());
    let mut legs: FuturesUnordered<LegFuture> = FuturesUnordered::new();
    legs.push(leg(
        0,
        params.client.clone(),
        params.url.clone(),
        params.spec.clone(),
        kind,
        Some(Arc::clone(&start_signal)),
    ));
    let stagger = tokio::time::sleep(stagger_for(ctx));
    tokio::pin!(stagger);
    let mut stagger_armed = true;
    let mut launched = false;
    let mut failed: Vec<usize> = Vec::new();
    // 起飞路径的错误优先作为最终错误（它是用户「默认会走」的路径）。
    let mut start_error: Option<DownloadError> = None;
    let mut other_error: Option<DownloadError> = None;

    loop {
        let launch_now = tokio::select! {
            biased;
            () = params.cancel_token.cancelled() => {
                return RaceOutcome { result: Err(DownloadError::Cancelled), failed, cancelled: true };
            }
            finished = legs.next() => {
                let Some((index, client, result)) = finished else {
                    break;
                };
                match result {
                    Ok(output) => {
                        // 备选胜出时起飞路径的探测阶梯可能仍在退避重试；只要它
                        // 已有一轮断在传输层，本次起飞就按「走不通」计（failover
                        // 语义、记失败、不再留作备选），而不是「只是慢」。
                        if index != 0
                            && !failed.contains(&0)
                            && start_signal.transport_failed.load(Ordering::Acquire)
                        {
                            failed.push(0);
                        }
                        return RaceOutcome { result: Ok((index, client, output)), failed, cancelled: false };
                    }
                    Err(error) => {
                        let transport = crate::net_failure::classify_download_error(&error).is_some();
                        if transport {
                            failed.push(index);
                        }
                        if index == 0 {
                            // 起飞路径未放出备选前就以 HTTP 层失败告终：链路可达，
                            // 失败归服务器，不竞速。
                            if !launched && !transport {
                                return RaceOutcome { result: Err(error), failed, cancelled: false };
                            }
                            start_error = Some(error);
                        } else if other_error.is_none() {
                            other_error = Some(error);
                        }
                        !launched
                    }
                }
            }
            () = start_signal.notify.notified(), if !launched => {
                if start_signal.transport_failed.load(Ordering::Acquire) {
                    true
                } else {
                    stagger_armed = !start_signal.answered.load(Ordering::Acquire);
                    false
                }
            }
            () = &mut stagger, if !launched && stagger_armed => true,
        };
        if launch_now && !launched {
            launched = true;
            for (offset, alt) in ctx.alternates.iter().enumerate() {
                match crate::downloader::build_client_with_tls_policy(
                    &alt.config,
                    &ctx.user_agent,
                    false,
                ) {
                    Ok(client) => legs.push(leg(
                        offset + 1,
                        client,
                        params.url.clone(),
                        params.spec.clone(),
                        kind,
                        None,
                    )),
                    Err(error) => log_info!(
                        "[auto-probe] task {} {:?} 路径 client 构建失败，跳过: {error:#}",
                        params.task_id,
                        alt.route
                    ),
                }
            }
            log_info!(
                "[auto-probe] task {} host {} 起飞路径 {:?} 未及时应答，放出 {} 条备选路径竞速",
                params.task_id,
                ctx.host,
                ctx.start_route,
                ctx.alternates.len()
            );
        }
    }
    let error = start_error
        .or(other_error)
        .unwrap_or_else(|| DownloadError::Other("auto probe race produced no result".to_string()));
    RaceOutcome {
        result: Err(error),
        failed,
        cancelled: false,
    }
}

fn leg_route(ctx: &AutoProxyCtx, index: usize) -> Option<RoutePath> {
    match index {
        0 => Some(ctx.start_route),
        n => ctx.alternates.get(n - 1).map(|alt| alt.route),
    }
}

/// 把断在传输层的路径记入跨任务失败权重。整机断网时所有路径都会失败，
/// 那与路径无关，不记。
fn record_failures(ctx: &AutoProxyCtx, failed: &[usize], db: &crate::db::Db) {
    if failed.is_empty() || !crate::route_health::network_reachable() {
        return;
    }
    for &index in failed {
        if let Some(route) = leg_route(ctx, index) {
            crate::route_health::record_path_failure(&ctx.host, route, db);
        }
    }
}

/// 纯函数：备选路径胜出后的新起飞标签。起飞路径确实断在传输层 → failover
/// 语义；起飞路径只是慢 → 实测（sampled）语义。
fn takeover_label(winner: RoutePath, start_failed: bool) -> &'static str {
    match winner {
        RoutePath::Direct | RoutePath::Link(_) => {
            if start_failed {
                route::DIRECT_FAILOVER
            } else {
                route::DIRECT_SAMPLED
            }
        }
        RoutePath::Proxy(source) => route::with_source(
            if start_failed {
                route::PROXY_FAILOVER
            } else {
                route::PROXY_SAMPLED
            },
            source,
        ),
    }
}

/// 纯函数：胜出路径接管后的备选集合——原起飞路径与其余备选（断在传输层的
/// 除外，本任务内不再浪费连接去试）。
fn remaining_alternates(
    ctx: &AutoProxyCtx,
    start_config: &ProxyConfig,
    winner: usize,
    failed: &[usize],
) -> Vec<PathCandidate> {
    let mut alternates = Vec::with_capacity(ctx.alternates.len());
    if !failed.contains(&0) {
        alternates.push(PathCandidate {
            route: ctx.start_route,
            config: start_config.clone(),
            prior_bps: ctx.start_prior_bps,
        });
    }
    for (offset, alt) in ctx.alternates.iter().enumerate() {
        let index = offset + 1;
        if index != winner && !failed.contains(&index) {
            alternates.push(alt.clone());
        }
    }
    alternates
}

async fn adopt_alternate(
    params: &mut DownloadParams,
    ctx: &AutoProxyCtx,
    winner: usize,
    client: Client,
    failed: &[usize],
) {
    let Some(alt) = ctx.alternates.get(winner - 1) else {
        return;
    };
    let label = takeover_label(alt.route, failed.contains(&0));
    let alternates = remaining_alternates(ctx, &params.proxy_config, winner, failed);
    log_info!(
        "[auto-probe] task {} host {} 改由 {:?} 起飞（{}），剩余备选 {:?}",
        params.task_id,
        ctx.host,
        alt.route,
        label,
        alternates.iter().map(|a| a.route).collect::<Vec<_>>()
    );
    params.client = client;
    params.proxy_config = alt.config.clone();
    if alt.route != RoutePath::Direct {
        // 多 CDN 钉定节点是直连 IP：起飞改走代理后不得再旁路代理。
        params.cdn.enabled = false;
    }
    crate::intercept::sync_egress(params);
    params.auto_proxy = Some(Arc::new(AutoProxyCtx {
        host: ctx.host.clone(),
        user_agent: ctx.user_agent.clone(),
        start_route: alt.route,
        start_prior_bps: alt.prior_bps,
        start_label: label,
        alternates,
    }));
    if let Err(error) = params.db.set_task_auto_route(&params.task_id, label).await {
        log_info!(
            "[auto-probe] task {} auto_route 落库失败: {error:#}",
            params.task_id
        );
    }
    params.sink.emit(EngineEvent::TaskRouteChanged {
        task_id: params.task_id.clone(),
        route: label.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_proxy::CandidateSource;

    fn ctx(start: RoutePath, alternates: Vec<RoutePath>) -> AutoProxyCtx {
        AutoProxyCtx {
            host: "h.example".to_string(),
            user_agent: String::new(),
            start_route: start,
            start_prior_bps: Some(1e6),
            start_label: route::DIRECT,
            alternates: alternates
                .into_iter()
                .map(|route| PathCandidate {
                    route,
                    config: ProxyConfig::default(),
                    prior_bps: None,
                })
                .collect(),
        }
    }

    const SYSTEM: RoutePath = RoutePath::Proxy(CandidateSource::System);
    const MANUAL: RoutePath = RoutePath::Proxy(CandidateSource::ManualFields);

    const OK_RESPONSE: &str =
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
    const NOT_FOUND: &str =
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// 本地假服务：每个连接读完请求头后回固定响应；`None` = 只接受不应答
    /// （黑洞式慢路径）。返回端口与连接计数。HTTP 代理收到的是绝对形式请求，
    /// 对它而言同样「读头 → 回响应」，因此同一个假服务也充当代理。
    async fn fake_server(
        reply: Option<&'static str>,
    ) -> std::io::Result<(u16, Arc<std::sync::atomic::AtomicUsize>)> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let mut seen = Vec::new();
                    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => seen.extend_from_slice(&buf[..n]),
                        }
                    }
                    match reply {
                        Some(reply) => {
                            if let Err(error) = socket.write_all(reply.as_bytes()).await {
                                tracing::debug!("fake server peer closed early: {error}");
                            }
                        }
                        None => tokio::time::sleep(Duration::from_secs(30)).await,
                    }
                });
            }
        });
        Ok((port, hits))
    }

    /// 绑定后立即释放的回环端口：连接必被拒绝。
    fn closed_port() -> std::io::Result<u16> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        Ok(listener.local_addr()?.port())
    }

    fn system_proxy_path(port: u16) -> PathCandidate {
        PathCandidate {
            route: SYSTEM,
            config: ProxyConfig {
                mode: crate::proxy_config::ProxyMode::Manual,
                proxy_type: crate::proxy_config::ProxyType::Http,
                host: "127.0.0.1".to_string(),
                port,
                ..ProxyConfig::default()
            },
            prior_bps: None,
        }
    }

    async fn race_params(
        url: &str,
        host: &str,
        alternates: Vec<PathCandidate>,
    ) -> Result<DownloadParams, Box<dyn std::error::Error>> {
        // 假代理与假源站都在环回上，须关闭引擎的环回直连才能走代理路径。
        crate::proxy_config::disable_loopback_bypass_for_tests();
        let mut params =
            crate::downloader::test_support::params_with_interceptor(url, crate::intercept::noop())
                .await?;
        params.auto_proxy = Some(Arc::new(AutoProxyCtx {
            host: host.to_string(),
            user_agent: String::new(),
            start_route: RoutePath::Direct,
            start_prior_bps: None,
            start_label: route::DIRECT,
            alternates,
        }));
        Ok(params)
    }

    /// 原始缺陷的端到端回归：直连被拒时不再等 3 轮退避重试（黑洞时约 63s）
    /// 后整任务失败，而是立即经系统代理拿到响应，并就地接管任务出口。
    #[tokio::test]
    async fn refused_direct_is_taken_over_by_proxy_without_waiting() -> TestResult {
        let (proxy_port, proxy_hits) = fake_server(Some(OK_RESPONSE)).await?;
        let url = format!("http://127.0.0.1:{}/f.bin", closed_port()?);
        let host = format!("race-refused-{proxy_port}.test");
        let mut params = race_params(&url, &host, vec![system_proxy_path(proxy_port)]).await?;

        let started = std::time::Instant::now();
        let probed = race_probe(&mut params, ProbeKind::Held).await?;
        let elapsed = started.elapsed();

        assert!(
            elapsed < STAGGER_COLD,
            "传输失败须立即放出备选: {elapsed:?}"
        );
        assert_eq!(probed.info.total_bytes, 5);
        assert!(probed.held.is_some(), "胜出路径的响应保留为首条数据连接");
        assert_eq!(proxy_hits.load(Ordering::SeqCst), 1);
        assert_eq!(params.proxy_config.port, proxy_port, "任务出口改走代理");
        let ctx = params.auto_proxy.as_ref().ok_or("ctx")?;
        assert_eq!(ctx.start_route, SYSTEM);
        assert_eq!(ctx.start_label, route::PROXY_FAILOVER_SYSTEM);
        assert!(ctx.alternates.is_empty(), "断在传输层的直连不再留作备选");
        let row = params
            .db
            .load_task_by_id("t-refresh")
            .await?
            .ok_or("task row")?;
        assert_eq!(row.auto_route, route::PROXY_FAILOVER_SYSTEM);
        if crate::route_health::network_reachable() {
            assert!(
                crate::route_health::path_blocked(&host, RoutePath::Direct),
                "直连失败须记入路径失败权重，下个任务不再以它起飞"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn silent_direct_yields_to_proxy_after_stagger_and_stays_alternate() -> TestResult {
        let (direct_port, _) = fake_server(None).await?;
        let (proxy_port, _) = fake_server(Some(OK_RESPONSE)).await?;
        let url = format!("http://127.0.0.1:{direct_port}/f.bin");
        let host = format!("race-silent-{proxy_port}.test");
        let mut params = race_params(&url, &host, vec![system_proxy_path(proxy_port)]).await?;

        let started = std::time::Instant::now();
        race_probe(&mut params, ProbeKind::Held).await?;
        let elapsed = started.elapsed();

        assert!(elapsed >= STAGGER_COLD, "错峰内不得抢跑备选: {elapsed:?}");
        assert!(
            elapsed < STAGGER_COLD * 3,
            "错峰到点即放出备选: {elapsed:?}"
        );
        let ctx = params.auto_proxy.as_ref().ok_or("ctx")?;
        assert_eq!(ctx.start_label, route::PROXY_SAMPLED_SYSTEM, "慢 ≠ 失败");
        let alternates: Vec<_> = ctx.alternates.iter().map(|a| a.route).collect();
        assert_eq!(alternates, vec![RoutePath::Direct], "只是慢的直连留作备选");
        assert!(!crate::route_health::path_blocked(&host, RoutePath::Direct));
        Ok(())
    }

    #[tokio::test]
    async fn server_answer_on_start_path_suppresses_race() -> TestResult {
        let (direct_port, _) = fake_server(Some(NOT_FOUND)).await?;
        let (proxy_port, proxy_hits) = fake_server(Some(OK_RESPONSE)).await?;
        let url = format!("http://127.0.0.1:{direct_port}/f.bin");
        let host = format!("race-404-{proxy_port}.test");
        let mut params = race_params(&url, &host, vec![system_proxy_path(proxy_port)]).await?;

        let result = race_probe(&mut params, ProbeKind::Held).await;

        assert!(result.is_err(), "404 是服务器结论，不是路径问题");
        assert_eq!(
            proxy_hits.load(Ordering::SeqCst),
            0,
            "起飞路径已拿到服务器应答（即便阶梯重试超过错峰时长）不得竞速"
        );
        assert_eq!(params.proxy_config.port, 0, "任务出口不变");
        Ok(())
    }

    #[test]
    fn takeover_label_distinguishes_failed_from_slow_start() {
        assert_eq!(takeover_label(SYSTEM, true), route::PROXY_FAILOVER_SYSTEM);
        assert_eq!(takeover_label(SYSTEM, false), route::PROXY_SAMPLED_SYSTEM);
        assert_eq!(takeover_label(MANUAL, true), route::PROXY_FAILOVER_MANUAL);
        assert_eq!(
            takeover_label(RoutePath::Direct, true),
            route::DIRECT_FAILOVER
        );
        assert_eq!(
            takeover_label(RoutePath::Direct, false),
            route::DIRECT_SAMPLED
        );
    }

    #[test]
    fn takeover_keeps_slow_start_but_drops_failed_paths() {
        let c = ctx(RoutePath::Direct, vec![MANUAL, SYSTEM]);
        // 系统代理胜出，直连只是慢：直连留作备选，手动代理同样保留。
        let slow: Vec<_> = remaining_alternates(&c, &ProxyConfig::default(), 2, &[])
            .into_iter()
            .map(|a| a.route)
            .collect();
        assert_eq!(slow, vec![RoutePath::Direct, MANUAL]);
        // 直连与手动代理都断在传输层：本任务不再浪费连接试它们。
        let failed: Vec<_> = remaining_alternates(&c, &ProxyConfig::default(), 2, &[0, 1])
            .into_iter()
            .map(|a| a.route)
            .collect();
        assert!(failed.is_empty(), "{failed:?}");
        // 原起飞路径保留它的先验，供池内播种。
        let kept = remaining_alternates(&c, &ProxyConfig::default(), 1, &[]);
        assert_eq!(kept[0].prior_bps, Some(1e6));
    }
}
