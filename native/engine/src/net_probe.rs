//! 按真实下载出口测量来源吞吐（`daemon.net.measureRoutes`，AI 助手 `fd_measure_routes`）。
//!
//! 每条路线用与真实下载相同的 HTTP client、请求头 / Cookie / 站点凭据与出口选择，多连接 Range
//! 拉取同一来源，数据读进内存即丢弃：不建任务、不写下载目录与数据库、不改设置。路线依次执行，
//! 互不争抢带宽；同一时刻只允许一次测量。计时拆分（DNS / 建连 / TLS）用独立的一次手工握手
//! 得出：reqwest 不暴露连接阶段耗时；经代理时计时对象是代理服务器。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE};
use reqwest::{Client, Method, StatusCode, Url};
use tokio_util::sync::CancellationToken;

use crate::download_manager::DownloadManager;
use crate::downloader::RequestSpec;
use crate::proxy_config::{ProxyConfig, ProxyMode, ProxyType};

/// 单次测量的路线数上限。
pub const MAX_ROUTES: usize = 4;
/// 并行连接数上限。
pub const MAX_CONNECTIONS: u32 = 32;
/// 未指定且任务为「自动线程数」时的连接数。
const DEFAULT_CONNECTIONS: u32 = 8;
/// 每条附加连接至少分到的来源跨度：来源太小时少开连接。
const MIN_SLICE_BYTES: u64 = 256 * 1024;
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TLS_TIMEOUT: Duration = Duration::from_secs(10);
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(20);
/// 单条路线在传输时长之外留给建连、握手与收尾的余量。
const ROUTE_OVERHEAD: Duration = Duration::from_secs(40);
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// 峰值滑动窗口 = 5 × 200 ms。
const PEAK_WINDOW_SAMPLES: usize = 5;

static MEASURING: AtomicBool = AtomicBool::new(false);

/// [`DownloadManager::probe_egress`] 的结果：任务起跑时会用的出口。
pub(crate) struct ProbeEgress {
    pub(crate) client: Client,
    pub(crate) proxy: ProxyConfig,
    pub(crate) route_label: String,
    pub(crate) router: Option<Arc<crate::rule_route::EgressRouter>>,
}

/// 一次测量请求。`task_id` 与 `url` 恰好给一个。
#[derive(Debug, Clone, Default)]
pub struct MeasureRequest {
    pub task_id: Option<String>,
    pub url: Option<String>,
    /// `current` / `direct` / `system` / `manual` 或不含凭据的代理 URL；空 = `current` + `direct`。
    pub routes: Vec<String>,
    pub connections: Option<u32>,
    pub max_bytes: u64,
    pub max_duration: Duration,
}

/// 被测来源与计划。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeasureReport {
    pub scheme: String,
    pub host: String,
    pub task_id: Option<String>,
    pub connections: u32,
    pub max_bytes: u64,
    pub max_seconds: u32,
    pub routes: Vec<RouteReport>,
}

/// 一条路线的结果（字段语义见 `fluxdown_protocol::NetRouteMeasureDto`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouteReport {
    pub route: String,
    pub egress: String,
    pub route_label: String,
    pub ok: bool,
    pub error: Option<String>,
    pub http_status: Option<u16>,
    pub timing_target: String,
    pub dns_ms: Option<u64>,
    pub connect_ms: Option<u64>,
    pub tls_ms: Option<u64>,
    pub first_byte_ms: Option<u64>,
    pub range_supported: Option<bool>,
    pub total_bytes: Option<u64>,
    pub bytes: u64,
    pub seconds: f64,
    pub avg_bytes_per_sec: u64,
    pub peak_bytes_per_sec: u64,
    pub connections_used: u32,
    pub per_connection_bytes_per_sec: Vec<u64>,
    pub stopped_by: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MeasureError {
    #[error("task not found")]
    NotFound,
    #[error("{0}")]
    InvalidArgument(String),
    #[error("another network measurement is already running")]
    Busy,
    #[error("{0}")]
    Failed(String),
}

/// 测量期间占用全局「测量中」标记，结束（含取消）时释放。
struct MeasuringGuard;

impl MeasuringGuard {
    fn acquire() -> Result<Self, MeasureError> {
        MEASURING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| MeasureError::Busy)
    }
}

impl Drop for MeasuringGuard {
    fn drop(&mut self) {
        MEASURING.store(false, Ordering::Release);
    }
}

/// 解析后的路线。
#[derive(Debug, Clone, PartialEq)]
enum RouteSpec {
    Current,
    Direct,
    System,
    Manual,
    /// 显式代理与其展示形态（`scheme://host:port`）。
    Proxy(String),
}

impl RouteSpec {
    fn parse(raw: &str) -> Result<Self, MeasureError> {
        let trimmed = raw.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "current" => return Ok(Self::Current),
            "direct" => return Ok(Self::Direct),
            "system" => return Ok(Self::System),
            "manual" => return Ok(Self::Manual),
            _ => {}
        }
        let url = Url::parse(trimmed)
            .map_err(|_| MeasureError::InvalidArgument(format!("unknown route `{trimmed}`")))?;
        let scheme = url.scheme().to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https" | "socks5" | "socks5h") {
            return Err(MeasureError::InvalidArgument(format!(
                "unsupported proxy scheme `{scheme}` (http, https, socks5, socks5h)"
            )));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(MeasureError::InvalidArgument(
                "proxy URL must not contain credentials".to_owned(),
            ));
        }
        let host = url
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| MeasureError::InvalidArgument("proxy URL has no host".to_owned()))?;
        let port = url.port_or_known_default().unwrap_or(1080);
        Ok(Self::Proxy(format!("{scheme}://{host}:{port}")))
    }

    fn name(&self) -> String {
        match self {
            Self::Current => "current".to_owned(),
            Self::Direct => "direct".to_owned(),
            Self::System => "system".to_owned(),
            Self::Manual => "manual".to_owned(),
            Self::Proxy(display) => display.clone(),
        }
    }
}

/// 被测来源：URL 与请求上下文（Cookie / Referer / 请求头只在本机使用）。
struct Source {
    url: Url,
    task_id: Option<String>,
    spec: RequestSpec,
    proxy_url: String,
    task_user_agent: String,
    queue_id: String,
    ignore_tls_errors: bool,
    connections: u32,
}

/// 一条路线的执行计划。
enum Plan {
    /// 出口已确定（`current`）。
    Egress {
        route: String,
        egress: ProbeEgress,
    },
    /// 固定出口配置，client 在执行时取。
    Config {
        route: String,
        proxy: ProxyConfig,
        label: String,
    },
    /// 系统代理在执行时探测（可能起子进程，放阻塞线程池）。
    System,
    Failed {
        route: String,
        error: String,
    },
}

impl DownloadManager {
    /// 准备一次测量：校验参数、读取任务请求上下文并确定各路线出口（不发网络请求），返回
    /// 不借用 manager 的测量 future，供宿主在 actor 之外执行。另一测量进行中时返回
    /// [`MeasureError::Busy`]。
    pub async fn measure_routes_future(
        &self,
        request: MeasureRequest,
    ) -> Result<impl Future<Output = MeasureReport> + Send + use<>, MeasureError> {
        let mut source = self.measure_source(&request).await?;
        if source.task_user_agent.is_empty() {
            source.spec.site_user_agent_rules =
                self.probe_site_user_agent_rules(source.task_id.as_deref());
        }
        let specs = if request.routes.is_empty() {
            vec![RouteSpec::Current, RouteSpec::Direct]
        } else {
            request
                .routes
                .iter()
                .map(|raw| RouteSpec::parse(raw))
                .collect::<Result<Vec<_>, _>>()?
        };
        if specs.len() > MAX_ROUTES {
            return Err(MeasureError::InvalidArgument(format!(
                "at most {MAX_ROUTES} routes per measurement"
            )));
        }
        let user_agent = self.probe_user_agent(&source.task_user_agent, &source.queue_id);
        let plans = specs
            .into_iter()
            .map(|spec| self.plan_route(spec, &source))
            .collect::<Vec<_>>();
        let guard = MeasuringGuard::acquire()?;
        let limits = Limits {
            max_bytes: request.max_bytes.max(1),
            max_duration: request.max_duration,
        };
        Ok(async move {
            let _guard = guard;
            run(source, plans, user_agent, limits).await
        })
    }

    async fn measure_source(&self, request: &MeasureRequest) -> Result<Source, MeasureError> {
        let task_id = request
            .task_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let url = request
            .url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty());
        let requested_connections = request
            .connections
            .map(|connections| connections.clamp(1, MAX_CONNECTIONS));
        match (task_id, url) {
            (Some(task_id), None) => {
                let db = self.database();
                let task = db
                    .load_task_by_id(task_id)
                    .await
                    .map_err(|error| MeasureError::Failed(format!("{error:#}")))?
                    .ok_or(MeasureError::NotFound)?;
                let url = parse_source_url(&task.url)?;
                let (cookies, referrer, headers) = crate::task_edit::request_context(db, &task)
                    .await
                    .map_err(|error| MeasureError::Failed(format!("{error:#}")))?;
                let persisted_ua = headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
                    .map(|(_, value)| value.clone())
                    .unwrap_or_else(|| task.properties.user_agent.clone());
                let task_connections = u32::try_from(task.segments)
                    .ok()
                    .filter(|segments| *segments > 0)
                    .unwrap_or(DEFAULT_CONNECTIONS);
                Ok(Source {
                    url,
                    task_id: Some(task.task_id.clone()),
                    spec: RequestSpec {
                        cookies,
                        referrer,
                        extra_headers: headers,
                        ..RequestSpec::default()
                    },
                    proxy_url: task.proxy_url.clone(),
                    task_user_agent: persisted_ua,
                    queue_id: task.queue_id.clone(),
                    ignore_tls_errors: task.ignore_tls_errors,
                    connections: requested_connections
                        .unwrap_or(task_connections)
                        .clamp(1, MAX_CONNECTIONS),
                })
            }
            (None, Some(url)) => Ok(Source {
                url: parse_source_url(url)?,
                task_id: None,
                spec: RequestSpec::default(),
                proxy_url: String::new(),
                task_user_agent: String::new(),
                queue_id: String::new(),
                ignore_tls_errors: false,
                connections: requested_connections.unwrap_or(DEFAULT_CONNECTIONS),
            }),
            _ => Err(MeasureError::InvalidArgument(
                "give exactly one of taskId / url".to_owned(),
            )),
        }
    }

    fn plan_route(&self, spec: RouteSpec, source: &Source) -> Plan {
        let route = spec.name();
        match spec {
            RouteSpec::Current => match self.probe_egress(
                source.url.as_str(),
                &source.proxy_url,
                &source.task_user_agent,
                &source.queue_id,
                source.ignore_tls_errors,
            ) {
                Ok(egress) => Plan::Egress { route, egress },
                Err(error) => Plan::Failed {
                    route,
                    error: format!("{error:#}"),
                },
            },
            RouteSpec::Direct => Plan::Config {
                route,
                proxy: ProxyConfig::default(),
                label: String::new(),
            },
            RouteSpec::System => Plan::System,
            RouteSpec::Manual => {
                let proxy = self.manual_proxy_config();
                if proxy.host.is_empty() || proxy.port == 0 {
                    Plan::Failed {
                        route,
                        error: "no manual proxy server is configured".to_owned(),
                    }
                } else {
                    Plan::Config {
                        route,
                        proxy,
                        label: String::new(),
                    }
                }
            }
            RouteSpec::Proxy(display) => Plan::Config {
                proxy: ProxyConfig::from_proxy_url(&display),
                route: display,
                label: String::new(),
            },
        }
    }
}

fn parse_source_url(raw: &str) -> Result<Url, MeasureError> {
    let url = Url::parse(raw.trim())
        .map_err(|_| MeasureError::InvalidArgument("source is not a valid URL".to_owned()))?;
    if !matches!(url.scheme(), "http" | "https" | "ftp") {
        return Err(MeasureError::InvalidArgument(format!(
            "only http(s) and ftp sources can be measured (got `{}`)",
            url.scheme()
        )));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(MeasureError::InvalidArgument(
            "source URL has no host".to_owned(),
        ));
    }
    Ok(url)
}

#[derive(Clone, Copy)]
struct Limits {
    max_bytes: u64,
    max_duration: Duration,
}

async fn run(
    source: Source,
    plans: Vec<Plan>,
    user_agent: String,
    limits: Limits,
) -> MeasureReport {
    let mut report = MeasureReport {
        scheme: source.url.scheme().to_owned(),
        host: source.url.host_str().unwrap_or_default().to_owned(),
        task_id: source.task_id.clone(),
        connections: source.connections,
        max_bytes: limits.max_bytes,
        max_seconds: u32::try_from(limits.max_duration.as_secs()).unwrap_or(u32::MAX),
        routes: Vec::with_capacity(plans.len()),
    };
    for plan in plans {
        let route = match plan {
            Plan::Failed { route, error } => RouteReport {
                route,
                error: Some(error),
                stopped_by: "error".to_owned(),
                ..RouteReport::default()
            },
            Plan::Egress { route, egress } => {
                let spec = RequestSpec {
                    egress: egress.router.clone(),
                    ..source.spec.clone()
                };
                let prepared = Prepared {
                    route,
                    client: egress.client,
                    proxy: egress.proxy,
                    label: egress.route_label,
                    spec,
                };
                measure_bounded(&source, prepared, limits).await
            }
            Plan::Config {
                route,
                proxy,
                label,
            } => match prepare_config(&source, route, proxy, label, &user_agent) {
                Ok(prepared) => measure_bounded(&source, prepared, limits).await,
                Err(report) => *report,
            },
            Plan::System => {
                let detected = tokio::task::spawn_blocking(|| {
                    ProxyConfig {
                        mode: ProxyMode::System,
                        ..ProxyConfig::default()
                    }
                    .resolve()
                })
                .await;
                match detected {
                    Ok(proxy) => match prepare_config(
                        &source,
                        "system".to_owned(),
                        proxy,
                        "system".to_owned(),
                        &user_agent,
                    ) {
                        Ok(prepared) => measure_bounded(&source, prepared, limits).await,
                        Err(report) => *report,
                    },
                    Err(error) => RouteReport {
                        route: "system".to_owned(),
                        error: Some(format!("system proxy detection failed: {error}")),
                        stopped_by: "error".to_owned(),
                        ..RouteReport::default()
                    },
                }
            }
        };
        report.routes.push(route);
    }
    report
}

/// 一条可执行的路线。
struct Prepared {
    route: String,
    client: Client,
    proxy: ProxyConfig,
    label: String,
    spec: RequestSpec,
}

fn prepare_config(
    source: &Source,
    route: String,
    proxy: ProxyConfig,
    label: String,
    user_agent: &str,
) -> Result<Prepared, Box<RouteReport>> {
    match crate::client_cache::get(
        &proxy,
        user_agent,
        source.ignore_tls_errors,
        crate::tls::HttpVersion::Http1,
    ) {
        Ok(client) => Ok(Prepared {
            route,
            client,
            proxy,
            label,
            spec: source.spec.clone(),
        }),
        Err(error) => Err(Box::new(RouteReport {
            egress: proxy.log_endpoint(),
            route,
            route_label: label,
            error: Some(format!("build client: {error:#}")),
            stopped_by: "error".to_owned(),
            ..RouteReport::default()
        })),
    }
}

async fn measure_bounded(source: &Source, prepared: Prepared, limits: Limits) -> RouteReport {
    let route = prepared.route.clone();
    let egress = prepared.proxy.log_endpoint();
    let label = prepared.label.clone();
    match tokio::time::timeout(
        limits.max_duration + ROUTE_OVERHEAD,
        measure_route(source, prepared, limits),
    )
    .await
    {
        Ok(report) => report,
        Err(_) => RouteReport {
            route,
            egress,
            route_label: label,
            error: Some("route measurement timed out".to_owned()),
            stopped_by: "error".to_owned(),
            ..RouteReport::default()
        },
    }
}

async fn measure_route(source: &Source, prepared: Prepared, limits: Limits) -> RouteReport {
    let mut report = RouteReport {
        route: prepared.route.clone(),
        egress: prepared.proxy.log_endpoint(),
        route_label: prepared.label.clone(),
        ..RouteReport::default()
    };
    if source.url.scheme() == "ftp" {
        measure_ftp(source, &prepared.proxy, &mut report).await;
        return report;
    }
    let host = source.url.host_str().unwrap_or_default().to_owned();
    let port = source.url.port_or_known_default().unwrap_or(80);
    let via_proxy = prepared.proxy.mode == ProxyMode::Manual
        && !prepared.proxy.host.is_empty()
        && prepared.proxy.port != 0;
    let timing = if via_proxy {
        report.timing_target = "proxy".to_owned();
        let tls =
            (prepared.proxy.proxy_type == ProxyType::Https).then(|| prepared.proxy.host.clone());
        handshake_timing(
            &prepared.proxy.host,
            prepared.proxy.port,
            tls,
            source.ignore_tls_errors,
        )
        .await
    } else {
        report.timing_target = "origin".to_owned();
        let tls = (source.url.scheme() == "https").then(|| host.clone());
        handshake_timing(&host, port, tls, source.ignore_tls_errors).await
    };
    report.dns_ms = timing.dns_ms;
    report.connect_ms = timing.connect_ms;
    report.tls_ms = timing.tls_ms;
    transfer(source, &prepared, limits, &mut report).await;
    if report.error.is_none()
        && let Some(error) = timing.error
        && report.bytes == 0
    {
        report.error = Some(error);
    }
    report
}

async fn measure_ftp(source: &Source, proxy: &ProxyConfig, report: &mut RouteReport) {
    report.timing_target = "origin".to_owned();
    report.connections_used = 1;
    let started = Instant::now();
    match crate::ftp_downloader::probe_ftp_bandwidth(
        source.url.as_str(),
        &CancellationToken::new(),
        proxy,
    )
    .await
    {
        Some(rate) if rate.is_finite() && rate > 0.0 => {
            report.ok = true;
            report.seconds = started.elapsed().as_secs_f64();
            // FTP 只取单连接短样本（引擎带宽探针的固定样本量）。
            report.avg_bytes_per_sec = rate as u64;
            report.peak_bytes_per_sec = report.avg_bytes_per_sec;
            report.per_connection_bytes_per_sec = vec![report.avg_bytes_per_sec];
            report.stopped_by = "sample".to_owned();
        }
        _ => {
            report.error = Some("FTP sample transfer failed".to_owned());
            report.stopped_by = "error".to_owned();
        }
    }
}

#[derive(Default)]
struct Timing {
    dns_ms: Option<u64>,
    connect_ms: Option<u64>,
    tls_ms: Option<u64>,
    error: Option<String>,
}

fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// 一次独立的 DNS → TCP → TLS 握手计时（不发 HTTP 请求）。
async fn handshake_timing(
    host: &str,
    port: u16,
    tls_server_name: Option<String>,
    ignore_tls_errors: bool,
) -> Timing {
    let mut timing = Timing::default();
    let started = Instant::now();
    let addr: SocketAddr =
        match tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((host, port))).await {
            Ok(Ok(mut addrs)) => match addrs.next() {
                Some(addr) => addr,
                None => {
                    timing.error = Some(format!("DNS returned no address for {host}"));
                    return timing;
                }
            },
            Ok(Err(error)) => {
                timing.error = Some(format!("DNS lookup failed for {host}: {error}"));
                return timing;
            }
            Err(_) => {
                timing.error = Some(format!("DNS lookup timed out for {host}"));
                return timing;
            }
        };
    timing.dns_ms = Some(millis(started.elapsed()));
    let started = Instant::now();
    let stream =
        match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                timing.error = Some(format!("TCP connect failed: {error}"));
                return timing;
            }
            Err(_) => {
                timing.error = Some("TCP connect timed out".to_owned());
                return timing;
            }
        };
    timing.connect_ms = Some(millis(started.elapsed()));
    let Some(server_name) = tls_server_name else {
        return timing;
    };
    let stream = match stream.into_std() {
        Ok(stream) => stream,
        Err(error) => {
            timing.error = Some(format!("TLS setup failed: {error}"));
            return timing;
        }
    };
    let handshake =
        tokio::task::spawn_blocking(move || tls_handshake(stream, server_name, ignore_tls_errors))
            .await;
    match handshake {
        Ok(Ok(elapsed)) => timing.tls_ms = Some(millis(elapsed)),
        Ok(Err(error)) => timing.error = Some(format!("TLS handshake failed: {error}")),
        Err(error) => timing.error = Some(format!("TLS handshake task failed: {error}")),
    }
    timing
}

fn tls_handshake(
    mut stream: std::net::TcpStream,
    server_name: String,
    ignore_tls_errors: bool,
) -> Result<Duration, String> {
    stream
        .set_nonblocking(false)
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(TLS_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(TLS_TIMEOUT))
        .map_err(|error| error.to_string())?;
    let config = crate::tls::client_config(crate::tls::HttpVersion::Http1, ignore_tls_errors)?;
    let name =
        rustls::pki_types::ServerName::try_from(server_name).map_err(|error| error.to_string())?;
    let mut connection =
        rustls::ClientConnection::new(Arc::new(config), name).map_err(|error| error.to_string())?;
    let started = Instant::now();
    while connection.is_handshaking() {
        connection
            .complete_io(&mut stream)
            .map_err(|error| error.to_string())?;
    }
    Ok(started.elapsed())
}

/// 单条连接的读取结果。
struct ConnOutcome {
    bytes: u64,
    error: Option<String>,
    eof: bool,
}

/// 多连接共享的计数与停止信号。
struct Shared {
    total: AtomicU64,
    stop: AtomicBool,
    budget_hit: AtomicBool,
    max_bytes: u64,
}

impl Shared {
    fn add(&self, bytes: u64) {
        let total = self.total.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if total >= self.max_bytes {
            self.budget_hit.store(true, Ordering::Relaxed);
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}

async fn read_body(
    mut response: reqwest::Response,
    shared: &Shared,
    deadline: tokio::time::Instant,
) -> ConnOutcome {
    let mut outcome = ConnOutcome {
        bytes: 0,
        error: None,
        eof: false,
    };
    while !shared.stop.load(Ordering::Relaxed) {
        match tokio::time::timeout_at(deadline, response.chunk()).await {
            Err(_) => break,
            Ok(Ok(Some(chunk))) => {
                let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
                outcome.bytes = outcome.bytes.saturating_add(len);
                shared.add(len);
            }
            Ok(Ok(None)) => {
                outcome.eof = true;
                break;
            }
            Ok(Err(error)) => {
                outcome.error = Some(format!("read failed: {error}"));
                break;
            }
        }
    }
    outcome
}

/// `Content-Range: bytes a-b/total` 的总大小。
fn content_range_total(response: &reqwest::Response) -> Option<u64> {
    let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let (_, total) = value.rsplit_once('/')?;
    total.trim().parse().ok()
}

fn content_length(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

async fn open_range(
    prepared: &Prepared,
    url: &str,
    offset: u64,
) -> Result<reqwest::Response, String> {
    let request =
        crate::downloader::build_request(&prepared.client, url, Method::GET, &prepared.spec)
            .header("Range", format!("bytes={offset}-"))
            .header("Accept-Encoding", "identity");
    match tokio::time::timeout(FIRST_BYTE_TIMEOUT, request.send()).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(format!("{error:#}")),
        Err(_) => Err("no response headers within 20 s".to_owned()),
    }
}

async fn transfer(source: &Source, prepared: &Prepared, limits: Limits, report: &mut RouteReport) {
    let url = source.url.as_str();
    let started = Instant::now();
    let first = match open_range(prepared, url, 0).await {
        Ok(response) => response,
        Err(error) => {
            report.error = Some(error);
            report.stopped_by = "error".to_owned();
            return;
        }
    };
    report.first_byte_ms = Some(millis(started.elapsed()));
    let status = first.status();
    report.http_status = Some(status.as_u16());
    if !status.is_success() {
        report.error = Some(format!("HTTP {status}"));
        report.stopped_by = "error".to_owned();
        return;
    }
    let ranged = status == StatusCode::PARTIAL_CONTENT;
    report.range_supported = Some(ranged);
    report.total_bytes = if ranged {
        content_range_total(&first)
    } else {
        content_length(&first)
    };
    let connections = match (ranged, report.total_bytes) {
        (true, Some(total)) if source.connections > 1 => {
            let by_size = (total / MIN_SLICE_BYTES).max(1);
            u32::try_from(by_size)
                .unwrap_or(u32::MAX)
                .min(source.connections)
        }
        _ => 1,
    };
    let shared = Shared {
        total: AtomicU64::new(0),
        stop: AtomicBool::new(false),
        budget_hit: AtomicBool::new(false),
        max_bytes: limits.max_bytes,
    };
    let transfer_started = Instant::now();
    let deadline = tokio::time::Instant::now() + limits.max_duration;
    let total_size = report.total_bytes.unwrap_or(0);
    let shared_ref = &shared;
    let mut readers = Vec::with_capacity(usize::try_from(connections).unwrap_or(1));
    readers.push(futures_util::future::Either::Left(read_body(
        first, shared_ref, deadline,
    )));
    for index in 1..connections {
        let offset = total_size / u64::from(connections) * u64::from(index);
        readers.push(futures_util::future::Either::Right(async move {
            match open_range(prepared, url, offset).await {
                Ok(response) if response.status() == StatusCode::PARTIAL_CONTENT => {
                    read_body(response, shared_ref, deadline).await
                }
                Ok(response) => ConnOutcome {
                    bytes: 0,
                    error: Some(format!("HTTP {}", response.status())),
                    eof: false,
                },
                Err(error) => ConnOutcome {
                    bytes: 0,
                    error: Some(error),
                    eof: false,
                },
            }
        }));
    }
    let done = AtomicBool::new(false);
    let sampler = sample_peak(&shared, &done);
    let readers = async {
        let outcomes = join_all(readers).await;
        done.store(true, Ordering::Relaxed);
        outcomes
    };
    let (peak, outcomes) = tokio::join!(sampler, readers);
    let seconds = transfer_started.elapsed().as_secs_f64();
    let bytes = shared.total.load(Ordering::Relaxed);
    report.bytes = bytes;
    report.seconds = seconds;
    report.avg_bytes_per_sec = per_second(bytes, seconds);
    report.peak_bytes_per_sec = peak.max(report.avg_bytes_per_sec);
    report.connections_used = u32::try_from(
        outcomes
            .iter()
            .filter(|outcome| outcome.bytes > 0 || outcome.error.is_none())
            .count(),
    )
    .unwrap_or(u32::MAX);
    report.per_connection_bytes_per_sec = outcomes
        .iter()
        .map(|outcome| per_second(outcome.bytes, seconds))
        .collect();
    let first_error = outcomes.iter().find_map(|outcome| outcome.error.clone());
    report.stopped_by = if shared.budget_hit.load(Ordering::Relaxed) {
        "bytes"
    } else if outcomes.iter().any(|outcome| outcome.eof) && bytes > 0 {
        "eof"
    } else if bytes == 0 && first_error.is_some() {
        "error"
    } else {
        "time"
    }
    .to_owned();
    report.ok = bytes > 0;
    if !report.ok {
        report.error = first_error.or_else(|| Some("no data received".to_owned()));
    } else if let Some(error) = first_error {
        // 部分连接失败不否定整体结果，但要让解读者知道连接数不足的原因。
        report.error = Some(format!("some connections failed: {error}"));
    }
}

fn per_second(bytes: u64, seconds: f64) -> u64 {
    if seconds <= 0.0 {
        return 0;
    }
    (bytes as f64 / seconds) as u64
}

/// 每 200 ms 取一次累计字节，返回 1 秒滑动窗口内的峰值速率。
async fn sample_peak(shared: &Shared, done: &AtomicBool) -> u64 {
    let mut samples = std::collections::VecDeque::with_capacity(PEAK_WINDOW_SAMPLES + 1);
    samples.push_back(0_u64);
    let mut peak = 0_u64;
    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.tick().await;
    while !done.load(Ordering::Relaxed) {
        interval.tick().await;
        let total = shared.total.load(Ordering::Relaxed);
        samples.push_back(total);
        if samples.len() > PEAK_WINDOW_SAMPLES + 1 {
            samples.pop_front();
        }
        if samples.len() == PEAK_WINDOW_SAMPLES + 1
            && let Some(oldest) = samples.front()
        {
            peak = peak.max(total.saturating_sub(*oldest));
        }
    }
    peak
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_parse_keywords_and_credential_free_proxies() {
        assert_eq!(RouteSpec::parse(" Current ").unwrap(), RouteSpec::Current);
        assert_eq!(RouteSpec::parse("manual").unwrap(), RouteSpec::Manual);
        assert_eq!(
            RouteSpec::parse("socks5h://127.0.0.1:7890").unwrap(),
            RouteSpec::Proxy("socks5h://127.0.0.1:7890".to_owned())
        );
        assert_eq!(
            RouteSpec::parse("http://proxy.local").unwrap(),
            RouteSpec::Proxy("http://proxy.local:80".to_owned())
        );
        assert!(RouteSpec::parse("http://user:pw@proxy.local:8080").is_err());
        assert!(RouteSpec::parse("ftp://proxy.local:21").is_err());
        assert!(RouteSpec::parse("fastest").is_err());
    }

    #[test]
    fn sources_must_be_http_or_ftp() {
        assert!(parse_source_url("https://example.com/a.bin").is_ok());
        assert!(parse_source_url("ftp://example.com/a.bin").is_ok());
        assert!(parse_source_url("magnet:?xt=urn:btih:abc").is_err());
        assert!(parse_source_url("torrent-file://local").is_err());
    }

    #[test]
    fn budget_hit_stops_all_readers() {
        let shared = Shared {
            total: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            budget_hit: AtomicBool::new(false),
            max_bytes: 10,
        };
        shared.add(6);
        assert!(!shared.stop.load(Ordering::Relaxed));
        shared.add(4);
        assert!(shared.stop.load(Ordering::Relaxed));
        assert!(shared.budget_hit.load(Ordering::Relaxed));
    }
}
