//! `EngineBridge` —— [`PluginBridge`] 实现：网络出口守卫 + storage + log + 重试意图。
//!
//! ## 网络出口守卫（防 SSRF）
//! 保护本机与 headless server 不被恶意/有 bug 的第三方插件访问内部服务。核心是
//! **单一判定函数** [`is_globally_routable_unicast`]，供三处复用（杜绝判定漂移）：
//! 1. 字面量 IP 前置校验（进入 reqwest 之前，挡 hyper-util 对字面量 IP 的短路）；
//! 2. 自定义 `reqwest::dns::Resolve`（解析后过滤，挡 DNS rebinding、消 TOCTOU）；
//! 3. 逐跳重定向 `Policy::custom`（手动重建 30 跳上限 + 每跳字面量 IP 校验）。
//!
//! ## 代理
//! - 每个请求的出口：命中站点规则（`flux.fetch` 按请求 URL，yt-dlp 按首个位置参数）→ 规则
//!   出口；否则调用级出口（[`super::call_egress::CallEgress`]，任务 / 订阅自带代理、源链接
//!   命中的规则、全局模式或 Auto 计划的一步）；没有调用级出口（常驻宿主、通知钩子）按全局
//!   规则 + 全局模式现场解析（System 每次检测，Auto 折算直连）。全局代理 / 规则变更由
//!   [`EngineBridge::set_proxy_config`] 替换，已在途的请求 / 子进程沿用发起时的出口（#401）。
//! - 走调用级 fallback 出口的请求记入 [`NetObserver`]（断在传输层 / 未完成），manager 据此
//!   在 Auto 下换路重跑、并豁免网络等待导致的超时熔断。
//! - 开发模式插件同样遵循规则与代理，但私网 / 本地域恒直连（[`DEV_PRIVATE_NO_PROXY`]）。
//! - 单次调用严格 per-call fetch 上限退化为**全局并发 fetch 上限**（对宿主保护更强）。
//! - 配置代理时 DNS 由代理侧解析，[`GuardResolver`] 不参与（hostname 级过滤失效；
//!   字面量 IP 前置校验与逐跳重定向校验仍然生效）。代理由用户显式配置，视为可信出口。
//!   DNS 应答全部是 198.18.0.0/15 fake-ip 时同理，视为 TUN 代理出口原样放行（见
//!   [`guard_filter`]）：真实目标由 TUN 代理软件解析。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::{Semaphore, mpsc};

use crate::db::Db;
use crate::logger::{log_error, log_info, log_warn};
use crate::multi_nic::is_fake_ip;
use crate::proxy_config::ProxyConfig;

use super::call_egress::{CallEgress, NetObserver};
use super::runtime::{
    BridgeHttpRequest, BridgeHttpResponse, FfmpegAvailability, FfmpegOutcome, FfmpegSpec,
    PluginBridge, PluginError, PluginLogLevel, YtdlpAvailability, YtdlpOutcome, YtdlpSpec,
};
use super::tool_path::apply_full_path_env;

/// 响应体上限（超限截断 + `truncated:true`）。
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// 单请求超时。
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// 全局并发 fetch 上限。
const MAX_CONCURRENT_FETCH: usize = 8;
/// 重定向跳数上限（Policy::custom 丢失 limited 默认保护，手动重建）。
const MAX_REDIRECTS: usize = 30;
/// 单值上限 64KB。
const MAX_STORAGE_VALUE: usize = 64 * 1024;
/// 单插件 storage 键数上限。
const MAX_STORAGE_KEYS: usize = 100;
/// 单条日志截断长度。
const MAX_LOG_BYTES: usize = 4 * 1024;
/// ffmpeg 单次调用默认超时（缺省 `timeoutMs` 时）。
const FFMPEG_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// ffmpeg 单次调用超时上限（裁剪 `timeoutMs`）。
const FFMPEG_MAX_TIMEOUT: Duration = Duration::from_secs(1800);
/// 全局并发 ffmpeg 进程上限（CPU/IO 密集，保守取 2）。
const MAX_CONCURRENT_FFMPEG: usize = 2;
/// ffmpeg 参数条数上限。
const MAX_FFMPEG_ARGS: usize = 512;
/// ffmpeg 单参数字节上限。
const MAX_FFMPEG_ARG_LEN: usize = 8 * 1024;
/// ffmpeg stdout 回传上限（够 ffprobe 式 JSON；超限截断）。
const FFMPEG_STDOUT_CAP: usize = 256 * 1024;
/// ffmpeg stderr 回传上限（超限截断）。
const FFMPEG_STDERR_CAP: usize = 64 * 1024;
/// yt-dlp 单次调用默认超时（缺省 `timeoutMs` 时；提取通常数秒，下载可长）。
const YTDLP_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// yt-dlp 单次调用超时上限（裁剪 `timeoutMs`）。
const YTDLP_MAX_TIMEOUT: Duration = Duration::from_secs(3600);
/// 全局并发 yt-dlp 进程上限。
const MAX_CONCURRENT_YTDLP: usize = 2;
/// yt-dlp 参数条数上限。
const MAX_YTDLP_ARGS: usize = 512;
/// yt-dlp 单参数字节上限。
const MAX_YTDLP_ARG_LEN: usize = 8 * 1024;
/// yt-dlp stdout 回传默认上限（`-J` 播放列表 JSON 可较大；超限截断）。
/// 截断发生在 `wait_with_output` 完成后，只限制插件返回/JSON 解析大小，不限制
/// 子进程输出采集期间的内存占用。
/// 4 MiB 对 YouTube 单视频不够：其 `-J` 常超 4 MiB（`automatic_captions` 单字段
/// 即可 ~4 MiB），截断导致插件 JSON 解析失败（"Unexpected end of JSON input"）。
/// 本上限本身即可解除已发布插件的该故障；插件另用 `--parse-metadata` 剔除重字段
/// （需插件重新发布才生效），此处默认提升到 16 MiB 作为其他站点大输出的兜底。
///
/// 运行期可覆盖（见 [`ytdlp_stdout_cap`]）：config `component.ytdlp.stdout_cap`
/// （全局，字节）、`plugin.<id>.ytdlp.stdout_cap`（单插件，优先）或环境变量
/// `FLUXDOWN_YTDLP_STDOUT_CAP`（改默认值，无人值守部署用）。
const YTDLP_STDOUT_CAP: usize = 16 * 1024 * 1024;
/// stdout 上限的允许区间：低于 1 MiB 会让正常 `-J` 频繁被截；高于 64 MiB 会
/// 放大插件返回值与 JSON 解析开销。越界一律夹到区间内（不报错，保证下载不因
/// 配置写错而失败）。
const YTDLP_STDOUT_CAP_MIN: usize = 1024 * 1024;
const YTDLP_STDOUT_CAP_MAX: usize = 64 * 1024 * 1024;
/// 全局 stdout 上限的 config 键；单插件键为 `plugin.<id>.ytdlp.stdout_cap`。
const CONFIG_YTDLP_STDOUT_CAP: &str = "component.ytdlp.stdout_cap";
/// yt-dlp stderr 回传上限（超限截断）。
const YTDLP_STDERR_CAP: usize = 256 * 1024;

/// flux.fs 单文件字节上限。
const MAX_FS_FILE_BYTES: usize = 8 * 1024 * 1024;
/// flux.fs 单插件工作区顶层文件总量上限。
const MAX_FS_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// flux.fs 单插件工作区顶层文件数上限。
const MAX_FS_FILES: usize = 100;
/// flux.fs 文件名长度上限。
const MAX_FS_NAME_LEN: usize = 255;

/// 插件工作区目录：`<data_dir>/plugins-work/<encoded_id>/`。flux.fs 与
/// flux.ytdlp 共用同一根（cwd 对齐），使插件经 flux.fs 物化的输入文件正好落在
/// 工具的工作目录里。目录名为 identity 的无碰撞编码：`[A-Za-z0-9-]` 原样保留，
/// 其余字节（含 `_`、`@`、`.`）一律转义为 `_XX`（两位小写十六进制）；`_` 自身
/// 必转义，故不同 identity 不会映射到同一目录。
pub(super) fn plugin_workspace(data_dir: &Path, plugin_id: &str) -> PathBuf {
    let mut safe_id = String::with_capacity(plugin_id.len());
    for b in plugin_id.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' {
            safe_id.push(b as char);
        } else {
            safe_id.push('_');
            safe_id.push_str(&format!("{b:02x}"));
        }
    }
    data_dir.join("plugins-work").join(safe_id)
}

/// flux.fs 文件名拒绝原因（`None` = 放行）。仅允许单层安全文件名：非空、
/// ≤`MAX_FS_NAME_LEN`、无路径分隔/盘符/NUL、非 `.`/`..`。
fn fs_name_reject_reason(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("空文件名");
    }
    if name.len() > MAX_FS_NAME_LEN {
        return Some("文件名过长");
    }
    if name == "." || name == ".." {
        return Some(". / .. 保留名");
    }
    if name.contains(['/', '\\', ':']) || name.contains('\0') {
        return Some("含路径分隔/盘符/NUL");
    }
    None
}

/// 唯一的「可全局路由单播」判定函数。三处复用，杜绝判定逻辑漂移。
///
/// 拒绝一切环回/私网/链路本地/文档/保留/元数据段等非公网单播地址。
///
/// # Examples
///
/// ```
/// use std::net::IpAddr;
/// use fluxdown_engine::plugin::bridge::is_globally_routable_unicast;
///
/// assert!(is_globally_routable_unicast("8.8.8.8".parse::<IpAddr>().unwrap()));
/// assert!(!is_globally_routable_unicast("127.0.0.1".parse::<IpAddr>().unwrap()));
/// assert!(!is_globally_routable_unicast("169.254.169.254".parse::<IpAddr>().unwrap()));
/// ```
pub fn is_globally_routable_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_v4_routable(v4),
        IpAddr::V6(v6) => is_v6_routable(v6),
    }
}

fn is_v4_routable(ip: Ipv4Addr) -> bool {
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_documentation()
    {
        return false;
    }
    let o = ip.octets();
    // 0.0.0.0/8 "this network"
    if o[0] == 0 {
        return false;
    }
    // 100.64.0.0/10 CGNAT
    if o[0] == 100 && (o[1] & 0xc0) == 0x40 {
        return false;
    }
    // 198.18.0.0/15 benchmarking
    if o[0] == 198 && (o[1] & 0xfe) == 18 {
        return false;
    }
    // 240.0.0.0/4 reserved
    if (o[0] & 0xf0) == 240 {
        return false;
    }
    true
}

fn is_v6_routable(ip: Ipv6Addr) -> bool {
    // IPv4-mapped ::ffff:a.b.c.d → 解包递归。
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_v4_routable(v4);
    }
    let seg = ip.segments();
    // 6to4 2002::/16 → 内嵌 IPv4 递归。
    if seg[0] == 0x2002 {
        let v4 = Ipv4Addr::new(
            (seg[1] >> 8) as u8,
            (seg[1] & 0xff) as u8,
            (seg[2] >> 8) as u8,
            (seg[2] & 0xff) as u8,
        );
        return is_v4_routable(v4);
    }
    // NAT64 64:ff9b::/96 → 末 32bit 递归。
    if seg[0] == 0x0064 && seg[1] == 0xff9b {
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            (seg[6] & 0xff) as u8,
            (seg[7] >> 8) as u8,
            (seg[7] & 0xff) as u8,
        );
        return is_v4_routable(v4);
    }
    if ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
    {
        return false;
    }
    // Teredo 2001:0000::/32
    if seg[0] == 0x2001 && seg[1] == 0x0000 {
        return false;
    }
    // documentation 2001:0db8::/32
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return false;
    }
    true
}

/// 守卫用自定义 DNS 解析器：解析后仅保留可全局路由的地址（过滤规则见 [`guard_filter`]）。
/// 解析出地址却全被过滤（内网主机名）时报 [`GuardError::BlockedHost`]——这是守卫拦截而不是
/// 网络故障，Auto 不得据此换代理重跑（代理侧解析会绕开守卫）。
pub(super) struct GuardResolver;

impl reqwest::dns::Resolve for GuardResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let resolved: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let kept = guard_filter(resolved)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            let iter: reqwest::dns::Addrs = Box::new(kept.into_iter());
            Ok(iter)
        })
    }
}

/// DNS 应答过滤：
/// - 含可全局路由地址：只保留这些（混合应答里的内网地址丢弃）；
/// - 全落在 198.18.0.0/15：这是 TUN 代理（Clash / sing-box 等）的 fake-ip 应答，该段正是
///   经 TUN 入隧道、由代理软件还原域名的地址，原样交给连接器；丢弃会让连接器拿到空地址表，
///   报 "tcp connect error: Network unreachable"，TUN 下直连必败；
/// - 其余全是内网 / 保留段：[`GuardError::BlockedHost`]；
/// - 空应答：原样返回空表（由连接器报错，与解析失败同属网络故障）。
fn guard_filter(resolved: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, GuardError> {
    if resolved.is_empty() {
        return Ok(resolved);
    }
    let routable: Vec<SocketAddr> = resolved
        .iter()
        .copied()
        .filter(|sa| is_globally_routable_unicast(sa.ip()))
        .collect();
    if !routable.is_empty() {
        return Ok(routable);
    }
    if resolved.iter().all(|sa| is_fake_ip(sa.ip())) {
        return Ok(resolved);
    }
    Err(GuardError::BlockedHost)
}

#[derive(Debug, thiserror::Error)]
enum GuardError {
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("blocked: non-routable redirect target")]
    BlockedRedirect,
    #[error("blocked: host resolves only to non-routable addresses")]
    BlockedHost,
}

/// 错误链里是否有守卫拦截（不是网络故障，不归因出口）。
fn is_guard_error(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(e) = current {
        if e.downcast_ref::<GuardError>().is_some() {
            return true;
        }
        current = e.source();
    }
    false
}

/// 守卫 client 缓存容量上限；超出后整体清空（键空间 = 出口 × 认证 × 开发模式，通常个位数）。
const GUARDED_CLIENT_CACHE_CAP: usize = 32;

/// 开发模式插件额外直连的私网 / 本地域：联调目标多在本机或局域网，不能被转发给代理出口。
/// 环回由 [`crate::proxy_config::proxy_bypass_list`] 恒直连。
const DEV_PRIVATE_NO_PROXY: &str = "10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,169.254.0.0/16,\
100.64.0.0/10,fc00::/7,fe80::/10,local,lan,home.arpa,internal";

/// 随全局代理整体替换的出口配置。
struct BridgeNet {
    /// 全局代理配置（带站点规则）。没有调用级出口的请求按它现场解析（System 每次检测）。
    global: ProxyConfig,
}

/// 请求选定的出口。
struct RequestRoute {
    /// 具体出口（`None` / `Manual` 模式）。
    proxy: ProxyConfig,
    /// 走的是调用的 fallback 出口（未命中站点规则）——换路判据只看这类请求。
    via_fallback: bool,
}

impl BridgeNet {
    /// 生效的站点规则：调用级出口自带（自带代理时为空），否则取全局。
    fn rules<'a>(
        &'a self,
        egress: Option<&'a CallEgress>,
    ) -> Option<&'a Arc<crate::proxy_rules::ProxyRuleSet>> {
        match egress {
            Some(egress) => egress.rules(),
            None => self.global.rules.as_ref(),
        }
    }

    /// 请求出口：命中站点规则 → 规则出口；否则调用级 fallback（无调用级出口时为全局模式
    /// 现场解析，Auto 即直连）。System 检测会阻塞，放进 `spawn_blocking`。`dev` = 开发模式
    /// 插件（私网 / 本地域另走直连）。
    async fn route(
        &self,
        egress: Option<&CallEgress>,
        url: &url::Url,
        dev: bool,
    ) -> Result<RequestRoute, PluginError> {
        if let Some(rule) = self
            .rules(egress)
            .and_then(|rules| rules.first_match_url(url))
        {
            let rule_egress = rule.egress().clone();
            let proxy = if rule_egress.needs_detection() {
                tokio::task::spawn_blocking(move || rule_egress.resolve())
                    .await
                    .map_err(|e| PluginError::Runtime(format!("解析站点规则出口失败: {e}")))?
            } else {
                rule_egress.resolve()
            };
            return Ok(RequestRoute {
                proxy,
                via_fallback: false,
            });
        }
        let proxy = match egress {
            Some(egress) => egress.fallback().clone(),
            None => resolve_global(&self.global).await?,
        };
        // 走代理的 fallback 下，命中直连名单（环回 / 用户排除 / 开发模式私网）的请求实际
        // 直连：它的失败不能记到这一步的代理头上。
        let bypassed = proxy.mode != crate::proxy_config::ProxyMode::None
            && url.host_str().is_some_and(|host| {
                let mut list = crate::proxy_config::proxy_bypass_list(&proxy.no_proxy_list);
                if dev {
                    list.push(',');
                    list.push_str(DEV_PRIVATE_NO_PROXY);
                }
                crate::proxy_config::host_matches_no_proxy(host, &list)
            });
        Ok(RequestRoute {
            proxy,
            via_fallback: !bypassed,
        })
    }

    /// yt-dlp 的 `--proxy` 值与是否走 fallback。第一个位置参数命中站点规则时取规则出口
    /// （直连为 `Some("")`）；否则取调用级 fallback：显式设定恒注入（直连注入空值强制
    /// 直连），非显式（全局 None / Auto 直连路径）直连时不注入，插件自带的 `--proxy` 仍可用。
    async fn ytdlp_proxy(
        &self,
        egress: Option<&CallEgress>,
        args: &[String],
    ) -> Result<(Option<String>, bool), PluginError> {
        if let Some(url) = ytdlp_first_positional_url(args)
            && self
                .rules(egress)
                .is_some_and(|rules| rules.first_match_url(&url).is_some())
        {
            let route = self.route(egress, &url, false).await?;
            let proxy_url = crate::egress::proxy_endpoint(&route.proxy)
                .map(|endpoint| endpoint.url)
                .unwrap_or_default();
            return Ok((Some(proxy_url), false));
        }
        let (proxy, explicit) = match egress {
            Some(egress) => (egress.fallback().clone(), egress.explicit()),
            None => (resolve_global(&self.global).await?, false),
        };
        let proxy_url = crate::egress::proxy_endpoint(&proxy).map(|endpoint| endpoint.url);
        let injected = match proxy_url {
            Some(url) => Some(url),
            None if explicit => Some(String::new()),
            None => None,
        };
        Ok((injected, true))
    }
}

/// 全局模式现场解析（System 检测可能起子进程，放进 `spawn_blocking`；Auto 折算直连）。
async fn resolve_global(global: &ProxyConfig) -> Result<ProxyConfig, PluginError> {
    if global.mode != crate::proxy_config::ProxyMode::System {
        return Ok(global.resolve());
    }
    let global = global.clone();
    tokio::task::spawn_blocking(move || global.resolve())
        .await
        .map_err(|e| PluginError::Runtime(format!("检测系统代理失败: {e}")))
}

type GuardedClientKey = (String, bool, bool);

/// 引擎侧 `PluginBridge` 实现。
pub struct EngineBridge {
    net: std::sync::RwLock<Arc<BridgeNet>>,
    /// 守卫 client 缓存：键 =（出口指纹, 认证同源跳转, 开发模式）。
    clients: std::sync::Mutex<HashMap<GuardedClientKey, reqwest::Client>>,
    db: Db,
    plugin_retry_tx: mpsc::UnboundedSender<(String, u64)>,
    fetch_sema: Arc<Semaphore>,
    /// 数据目录：供 `flux.ffmpeg` 解析生效 ffmpeg 路径（manual→managed→system）。
    data_dir: PathBuf,
    /// 全局并发 ffmpeg 进程限流。
    ffmpeg_sema: Arc<Semaphore>,
    /// 全局并发 yt-dlp 进程限流。
    ytdlp_sema: Arc<Semaphore>,
}

impl EngineBridge {
    /// 构造带守卫的 bridge，出口按 `proxy`（带站点规则的全局配置）装配。坏代理配置在此失败。
    pub fn new(
        db: Db,
        proxy: &ProxyConfig,
        plugin_retry_tx: mpsc::UnboundedSender<(String, u64)>,
        data_dir: PathBuf,
    ) -> Result<Self, PluginError> {
        let bridge = Self {
            net: std::sync::RwLock::new(Arc::new(BridgeNet {
                global: proxy.clone(),
            })),
            clients: std::sync::Mutex::new(HashMap::new()),
            db,
            plugin_retry_tx,
            fetch_sema: Arc::new(Semaphore::new(MAX_CONCURRENT_FETCH)),
            data_dir,
            ffmpeg_sema: Arc::new(Semaphore::new(MAX_CONCURRENT_FFMPEG)),
            ytdlp_sema: Arc::new(Semaphore::new(MAX_CONCURRENT_YTDLP)),
        };
        bridge.guarded_client(&proxy.resolve(), false, false)?;
        Ok(bridge)
    }

    /// 全局代理或站点规则变更：之后发起的请求按新配置选出口；在途请求 / 子进程沿用
    /// 发起时的出口。全局出口 client 构建失败（坏代理地址）时保留原配置并返回错误。
    /// 会阻塞（System 模式检测系统代理），与下载侧全局 client 重建同在 actor 上调用。
    pub fn set_proxy_config(&self, proxy: &ProxyConfig) -> Result<(), PluginError> {
        self.guarded_client(&proxy.resolve(), false, false)?;
        *self
            .net
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(BridgeNet {
            global: proxy.clone(),
        });
        Ok(())
    }

    /// 当前全局代理配置（带站点规则），供插件调用制定出口计划。
    #[must_use]
    pub fn proxy_config(&self) -> ProxyConfig {
        self.net().global.clone()
    }

    fn net(&self) -> Arc<BridgeNet> {
        Arc::clone(
            &self
                .net
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn cached_client(&self, key: &GuardedClientKey) -> Option<reqwest::Client> {
        self.clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    fn cache_client(&self, key: GuardedClientKey, client: &reqwest::Client) {
        let mut clients = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if clients.len() >= GUARDED_CLIENT_CACHE_CAP {
            clients.clear();
        }
        clients.insert(key, client.clone());
    }

    /// 同步取（或构建并缓存）守卫 client：只用于装配期 / 设置变更时的快速失败校验。
    fn guarded_client(
        &self,
        proxy: &ProxyConfig,
        same_origin_redirects_only: bool,
        dev: bool,
    ) -> Result<reqwest::Client, PluginError> {
        let key = client_key(proxy, same_origin_redirects_only, dev);
        if let Some(client) = self.cached_client(&key) {
            return Ok(client);
        }
        let client = build_client(
            crate::egress::proxy_endpoint(proxy).as_ref(),
            same_origin_redirects_only,
            dev,
        )?;
        self.cache_client(key, &client);
        Ok(client)
    }

    /// 请求路径上取守卫 client：未命中缓存时在阻塞池构建（加载根证书等开销不占插件
    /// runtime 工作线程）。
    async fn request_client(
        &self,
        proxy: &ProxyConfig,
        same_origin_redirects_only: bool,
        dev: bool,
    ) -> Result<reqwest::Client, PluginError> {
        let key = client_key(proxy, same_origin_redirects_only, dev);
        if let Some(client) = self.cached_client(&key) {
            return Ok(client);
        }
        let endpoint = crate::egress::proxy_endpoint(proxy);
        let client = tokio::task::spawn_blocking(move || {
            build_client(endpoint.as_ref(), same_origin_redirects_only, dev)
        })
        .await
        .map_err(|e| PluginError::Runtime(format!("构建 fetch Client 任务失败: {e}")))??;
        self.cache_client(key, &client);
        Ok(client)
    }
}

fn client_key(
    proxy: &ProxyConfig,
    same_origin_redirects_only: bool,
    dev: bool,
) -> GuardedClientKey {
    (
        crate::client_cache::proxy_fingerprint(proxy),
        same_origin_redirects_only,
        dev,
    )
}

/// 插件 `flux.fetch` 用 client。非开发模式挂 [`GuardResolver`]、逐跳拦截非公网 IP（防 SSRF）；
/// 开发模式（「从目录安装」）放开私网 / 环回以便联调，同样遵循代理设定，但私网与本地域
/// 恒直连（[`DEV_PRIVATE_NO_PROXY`]）。超时、最大跳数、认证同源跳转规则一致。
fn build_client(
    proxy: Option<&crate::egress::ProxyEndpoint>,
    same_origin_redirects_only: bool,
    allow_private: bool,
) -> Result<reqwest::Client, PluginError> {
    let label = match (allow_private, same_origin_redirects_only) {
        (false, false) => "守卫",
        (false, true) => "认证守卫",
        (true, false) => "开发模式",
        (true, true) => "开发模式认证",
    };
    let mut builder = crate::downloader::client_builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error(GuardError::TooManyRedirects);
            }
            if same_origin_redirects_only
                && let Some(previous) = attempt.previous().first()
                && (previous.scheme() != attempt.url().scheme()
                    || previous.host_str() != attempt.url().host_str()
                    || previous.port_or_known_default() != attempt.url().port_or_known_default())
            {
                return attempt.stop();
            }
            if !allow_private && let Some(host) = attempt.url().host_str() {
                let trimmed = host.trim_matches(|c| c == '[' || c == ']');
                if let Ok(ip) = trimmed.parse::<IpAddr>()
                    && !is_globally_routable_unicast(ip)
                {
                    return attempt.error(GuardError::BlockedRedirect);
                }
            }
            attempt.follow()
        }));
    if !allow_private {
        builder = builder.dns_resolver(Arc::new(GuardResolver));
    }

    // 无显式代理时必须 no_proxy：否则 reqwest 会静默继承环境/系统代理，请求改由
    // 代理解析主机名，GuardResolver 的 DNS 级内网过滤随之失效。坏代理地址报错，
    // 绝不静默直连（会把本该走代理的流量放到明网上）。
    builder = match proxy {
        Some(endpoint) => {
            let no_proxy = if allow_private {
                format!("{},{DEV_PRIVATE_NO_PROXY}", endpoint.no_proxy)
            } else {
                endpoint.no_proxy.clone()
            };
            let proxy = reqwest::Proxy::all(&endpoint.url)
                .map_err(|e| PluginError::Runtime(format!("{label} Client 代理地址无效: {e}")))?;
            builder.proxy(proxy.no_proxy(reqwest::NoProxy::from_string(&no_proxy)))
        }
        None => builder.no_proxy(),
    };

    builder
        .build()
        .map_err(|e| PluginError::Runtime(format!("构建{label} Client 失败: {e}")))
}

fn collect_response_headers(headers: &reqwest::header::HeaderMap) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for (key, value) in headers {
        if let Ok(value) = value.to_str() {
            result
                .entry(key.as_str().to_string())
                .and_modify(|existing: &mut String| {
                    existing.push('\n');
                    existing.push_str(value);
                })
                .or_insert_with(|| value.to_string());
        }
    }
    result
}

/// `authRef` 空串视同未提供（M-5）。抽成独立函数只为不依赖网络就能单测：
/// `http_request` 内联判定需要真实 SSRF 守卫环境才能触发调用，纯逻辑
/// 判定不需要。
fn normalize_explicit_auth_ref(auth_ref: Option<String>) -> Option<String> {
    auth_ref.filter(|value| !value.is_empty())
}

#[async_trait::async_trait]
impl PluginBridge for EngineBridge {
    async fn http_request(
        &self,
        plugin_id: &str,
        mut req: BridgeHttpRequest,
    ) -> Result<BridgeHttpResponse, PluginError> {
        // scheme 仅 http/https。
        let parsed = url::Url::parse(&req.url)
            .map_err(|e| PluginError::InvalidOutput(format!("URL 非法: {e}")))?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(PluginError::InvalidOutput(
                "flux.fetch 仅支持 http/https".to_string(),
            ));
        }
        // 字面量 IP 前置校验（挡 hyper-util 对字面量 IP 的短路）。
        if !req.private_network_allowed
            && let Some(host) = parsed.host_str()
        {
            let trimmed = host.trim_matches(|c| c == '[' || c == ']');
            if let Ok(ip) = trimmed.parse::<IpAddr>()
                && !is_globally_routable_unicast(ip)
            {
                return Err(PluginError::InvalidOutput(
                    "blocked: non-routable IP".to_string(),
                ));
            }
        }

        // 空串视同未提供（M-5）：`{"authRef":""}` 反序列化为 `Some("")`，不是
        // `None`；`ctx.authRef` 本就可能是空串（PluginManager::resolve 对
        // magnet/ed2k/ftp 等 URL 拿不到站点键时不填充，authenticate() 未传
        // site 时也是空串），插件把它原样透传给 flux.fetch 不该被当成「显式
        // 声明了一个空引用」进而 fail-closed。
        req.auth_ref = normalize_explicit_auth_ref(req.auth_ref);
        let explicit_auth_ref = req.auth_ref.is_some();
        let mut auth_applied = false;
        let auth_ref = req
            .auth_ref
            .clone()
            .or_else(|| crate::auth::default_auth_ref(plugin_id, &req.url));
        if explicit_auth_ref && !req.auth_allowed {
            return Err(PluginError::Runtime(
                "插件未声明 auth 权限，不能使用认证凭据".to_string(),
            ));
        }
        if req.auth_allowed
            && let Some(auth_ref) = auth_ref
        {
            match crate::auth::load(&self.db, &auth_ref)
                .await
                .map_err(|e| PluginError::Runtime(format!("读取认证凭据失败: {e:#}")))?
            {
                Some(profile) => {
                    if profile.plugin_id != plugin_id {
                        return Err(PluginError::Runtime("认证凭据不属于当前插件".to_string()));
                    }
                    if crate::auth::site_key(&req.url).as_deref() != Some(profile.site.as_str()) {
                        return Err(PluginError::Runtime(
                            "认证凭据不属于当前请求站点".to_string(),
                        ));
                    }
                    let profile_valid = profile.is_valid_at(crate::auth::now_unix());
                    if !profile_valid && explicit_auth_ref {
                        return Err(PluginError::Runtime(format!(
                            "authentication_required: 认证凭据已过期 {auth_ref}"
                        )));
                    }
                    if profile_valid {
                        profile.apply_to_headers(&mut req.headers);
                        auth_applied = true;
                    }
                }
                None if explicit_auth_ref => {
                    return Err(PluginError::Runtime(format!(
                        "authentication_required: 未找到认证凭据 {auth_ref}"
                    )));
                }
                None => {}
            }
        }

        let permit = self
            .fetch_sema
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PluginError::Runtime("fetch semaphore closed".to_string()))?;
        let _permit = permit;

        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| PluginError::InvalidOutput(format!("HTTP method 非法: {}", req.method)))?;
        let net = self.net();
        let route = net
            .route(req.egress.as_deref(), &parsed, req.private_network_allowed)
            .await?;
        let client = self
            .request_client(&route.proxy, auth_applied, req.private_network_allowed)
            .await?;
        let mut rb = client.request(method, parsed);
        for (k, v) in &req.headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                rb = rb.header(name, value);
            }
        }
        if let Some(body) = req.body {
            rb = rb.body(body);
        }

        // 观测：调用结束时仍未完成（墙钟超时丢弃）或断在传输层的请求，是换路与熔断豁免的判据。
        let op = req
            .egress
            .as_ref()
            .map(|egress| NetObserver::begin(egress, route.via_fallback));
        let mut resp = match rb.send().await {
            Ok(resp) => resp,
            Err(e) => {
                let failure = (!is_guard_error(&e))
                    .then(|| crate::net_failure::classify(&e))
                    .flatten();
                if let Some(op) = op {
                    op.finish(failure.is_some());
                }
                return Err(PluginError::Runtime(crate::net_failure::append_tag(
                    format!("fetch 失败: {}", reqwest_error_detail(&e)),
                    failure,
                )));
            }
        };
        let status = resp.status().as_u16();
        // HeaderMap 允许同名响应头（尤其是多个 Set-Cookie）；wire 层以换行
        // 拼接，避免登录插件丢掉除最后一个之外的 Cookie。
        let headers = collect_response_headers(resp.headers());

        let mut body = Vec::new();
        let mut truncated = false;
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_BODY_BYTES {
                        let take = MAX_BODY_BYTES - body.len();
                        body.extend_from_slice(&chunk[..take]);
                        truncated = true;
                        break;
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => {
                    let failure = crate::net_failure::classify(&e);
                    if let Some(op) = op {
                        op.finish(failure.is_some());
                    }
                    return Err(PluginError::Runtime(crate::net_failure::append_tag(
                        format!("读取响应体失败: {e:#}"),
                        failure,
                    )));
                }
            }
        }
        if let Some(op) = op {
            op.finish(false);
        }

        Ok(BridgeHttpResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&body).to_string(),
            truncated,
        })
    }

    async fn auth_get(
        &self,
        plugin_id: &str,
        auth_ref: &str,
    ) -> Result<Option<crate::auth::AuthProfile>, PluginError> {
        let Some(profile) = crate::auth::load(&self.db, auth_ref)
            .await
            .map_err(|e| PluginError::Runtime(format!("读取认证凭据失败: {e:#}")))?
        else {
            return Ok(None);
        };
        if profile.plugin_id != plugin_id {
            return Err(PluginError::Runtime("认证凭据不属于当前插件".to_string()));
        }
        Ok(Some(profile))
    }

    async fn auth_save(
        &self,
        plugin_id: &str,
        mut profile: crate::auth::AuthProfile,
    ) -> Result<String, PluginError> {
        if profile.site.trim().is_empty() {
            return Err(PluginError::InvalidOutput(
                "认证档案必须提供 site".to_string(),
            ));
        }
        profile.plugin_id = plugin_id.to_string();
        profile.site = crate::auth::normalize_site(&profile.site).ok_or_else(|| {
            PluginError::InvalidOutput("认证档案的 site 必须是有效的 URL 或 host".to_string())
        })?;
        let canonical_auth_ref = format!("{plugin_id}::{}", profile.site);
        if profile.auth_ref.is_empty() {
            profile.auth_ref = canonical_auth_ref;
        } else if profile.auth_ref != canonical_auth_ref {
            return Err(PluginError::InvalidOutput(
                "authRef 必须等于当前插件和规范化站点组成的引用".to_string(),
            ));
        }
        let size = serde_json::to_vec(&profile)
            .map_err(|e| PluginError::Runtime(format!("序列化认证档案失败: {e}")))?
            .len();
        if size > MAX_STORAGE_VALUE {
            return Err(PluginError::InvalidOutput(format!(
                "认证档案超过 {MAX_STORAGE_VALUE} 字节上限"
            )));
        }
        crate::auth::save(&self.db, &profile)
            .await
            .map_err(|e| PluginError::Runtime(format!("保存认证凭据失败: {e:#}")))?;
        Ok(profile.auth_ref)
    }

    async fn auth_remove(&self, plugin_id: &str, auth_ref: &str) -> Result<(), PluginError> {
        if !auth_ref.starts_with(&format!("{plugin_id}::")) {
            return Err(PluginError::InvalidOutput(
                "authRef 必须属于当前插件".to_string(),
            ));
        }
        crate::auth::remove(&self.db, auth_ref)
            .await
            .map_err(|e| PluginError::Runtime(format!("删除认证凭据失败: {e:#}")))
    }

    async fn storage_get(&self, plugin_id: &str, key: &str) -> Option<String> {
        let full = format!("plugin.{plugin_id}.kv.{key}");
        self.db.get_config(&full).await.ok().flatten()
    }

    async fn storage_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: String,
    ) -> Result<(), PluginError> {
        if value.len() > MAX_STORAGE_VALUE {
            return Err(PluginError::InvalidOutput(format!(
                "storage 值超过 {MAX_STORAGE_VALUE} 字节上限"
            )));
        }
        let prefix = format!("plugin.{plugin_id}.kv.");
        let full = format!("{prefix}{key}");
        // 键数上限：仅当是新键时才计数。
        if let Ok(existing) = self.db.list_config_with_prefix(&prefix).await {
            let is_new = !existing.iter().any(|(k, _)| k == &full);
            if is_new && existing.len() >= MAX_STORAGE_KEYS {
                return Err(PluginError::InvalidOutput(format!(
                    "storage 键数超过 {MAX_STORAGE_KEYS} 上限"
                )));
            }
        }
        self.db
            .set_config(&full, &value)
            .await
            .map_err(|e| PluginError::Runtime(format!("storage 写入失败: {e}")))
    }

    async fn fs_write(
        &self,
        plugin_id: &str,
        name: &str,
        content: String,
    ) -> Result<(), PluginError> {
        if let Some(r) = fs_name_reject_reason(name) {
            return Err(PluginError::InvalidOutput(format!(
                "flux.fs 文件名 '{name}' 非法: {r}"
            )));
        }
        if content.len() > MAX_FS_FILE_BYTES {
            return Err(PluginError::InvalidOutput(format!(
                "flux.fs 文件超过 {MAX_FS_FILE_BYTES} 字节上限"
            )));
        }
        let ws = plugin_workspace(&self.data_dir, plugin_id);
        tokio::fs::create_dir_all(&ws)
            .await
            .map_err(|e| PluginError::Runtime(format!("flux.fs 创建工作区失败: {e}")))?;
        // 容量/文件数上限：统计顶层文件（跳过 `.cache` 等子目录）。
        let (mut count, mut total, mut existing, mut found) = (0usize, 0u64, 0u64, false);
        if let Ok(mut rd) = tokio::fs::read_dir(&ws).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                if e.file_type().await.map(|t| t.is_file()).unwrap_or(false) {
                    count += 1;
                    let sz = e.metadata().await.map(|m| m.len()).unwrap_or(0);
                    total += sz;
                    if e.file_name().to_string_lossy() == name {
                        found = true;
                        existing = sz;
                    }
                }
            }
        }
        if !found && count >= MAX_FS_FILES {
            return Err(PluginError::InvalidOutput(format!(
                "flux.fs 文件数超过 {MAX_FS_FILES} 上限"
            )));
        }
        if total - existing + content.len() as u64 > MAX_FS_TOTAL_BYTES {
            return Err(PluginError::InvalidOutput(format!(
                "flux.fs 工作区总量超过 {MAX_FS_TOTAL_BYTES} 字节上限"
            )));
        }
        let path = ws.join(name);
        // 权限必须在写入敏感内容前收紧；旧文件也不能沿用宽松权限。
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&path)
            .await
            .map_err(|e| PluginError::Runtime(format!("flux.fs 打开失败: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|e| PluginError::Runtime(format!("flux.fs 设置安全权限失败: {e}")))?;
        }
        use tokio::io::AsyncWriteExt;
        file.set_len(0)
            .await
            .map_err(|e| PluginError::Runtime(format!("flux.fs 截断失败: {e}")))?;
        file.write_all(content.as_bytes())
            .await
            .map_err(|e| PluginError::Runtime(format!("flux.fs 写入失败: {e}")))?;
        file.flush()
            .await
            .map_err(|e| PluginError::Runtime(format!("flux.fs flush 失败: {e}")))?;
        Ok(())
    }

    async fn fs_read(&self, plugin_id: &str, name: &str) -> Option<String> {
        if fs_name_reject_reason(name).is_some() {
            return None;
        }
        let path = plugin_workspace(&self.data_dir, plugin_id).join(name);
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                // 读上限保护：超上限截断（避免把超大文件读回 JS）。
                let capped = if bytes.len() > MAX_FS_FILE_BYTES {
                    &bytes[..MAX_FS_FILE_BYTES]
                } else {
                    &bytes[..]
                };
                Some(String::from_utf8_lossy(capped).into_owned())
            }
            Err(_) => None,
        }
    }

    async fn fs_remove(&self, plugin_id: &str, name: &str) -> Result<(), PluginError> {
        if let Some(r) = fs_name_reject_reason(name) {
            return Err(PluginError::InvalidOutput(format!(
                "flux.fs 文件名 '{name}' 非法: {r}"
            )));
        }
        let path = plugin_workspace(&self.data_dir, plugin_id).join(name);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PluginError::Runtime(format!("flux.fs 删除失败: {e}"))),
        }
    }

    async fn fs_list(&self, plugin_id: &str) -> Vec<String> {
        let ws = plugin_workspace(&self.data_dir, plugin_id);
        let mut out = Vec::new();
        if let Ok(mut rd) = tokio::fs::read_dir(&ws).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                if e.file_type().await.map(|t| t.is_file()).unwrap_or(false) {
                    out.push(e.file_name().to_string_lossy().into_owned());
                }
            }
        }
        out
    }

    fn log(&self, plugin_id: &str, level: PluginLogLevel, message: &str) {
        let truncated = if message.len() > MAX_LOG_BYTES {
            // 按字符边界安全截断。
            let mut end = MAX_LOG_BYTES;
            while end > 0 && !message.is_char_boundary(end) {
                end -= 1;
            }
            &message[..end]
        } else {
            message
        };
        match level {
            PluginLogLevel::Error => log_error!("[plugin:{}] {}", plugin_id, truncated),
            _ => log_info!("[plugin:{}] {}", plugin_id, truncated),
        }
    }

    fn request_retry(&self, _plugin_id: &str, task_id: &str, delay_ms: u64) {
        // fire-and-forget；限流在 actor 侧（max_auto_retries）。
        if self
            .plugin_retry_tx
            .send((task_id.to_string(), delay_ms))
            .is_err()
        {
            // actor 关闭后不再处理重试，属于引擎退出的正常生命周期。
            tracing::debug!("plugin retry actor closed");
        }
    }

    async fn record_artifact(
        &self,
        plugin_id: &str,
        task_id: &str,
        file_name: &str,
    ) -> Result<(), PluginError> {
        // 仅接受单层裸文件名（无路径分隔/盘符/`..`）；删除侧还有
        // `is_safe_file_name` 二次校验，双保险。
        let bad = file_name.is_empty()
            || file_name.len() > 512
            || file_name.contains(['/', '\\', ':'])
            || file_name == "."
            || file_name == "..";
        if bad {
            return Err(PluginError::Runtime(format!(
                "recordArtifact: 非法产物文件名: {file_name:?}"
            )));
        }
        // 产物必须是任务 save_dir 下已存在、且与任务主文件同 stem 起头的文件：
        // 否则插件可把同目录任意无关文件登记为产物，随「删除任务并删除文件」被删。
        let task = self
            .db
            .load_task_by_id(task_id)
            .await
            .map_err(|e| PluginError::Runtime(format!("recordArtifact 读取任务失败: {e}")))?
            .ok_or_else(|| PluginError::Runtime("recordArtifact: 任务不存在".to_string()))?;
        let stem = Path::new(&task.file_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if stem.is_empty() || !file_name.starts_with(stem) {
            return Err(PluginError::Runtime(format!(
                "recordArtifact: 产物须以任务文件名主干 '{stem}' 开头: {file_name:?}"
            )));
        }
        let is_file = tokio::fs::metadata(Path::new(&task.save_dir).join(file_name))
            .await
            .map(|m| m.is_file())
            .unwrap_or(false);
        if !is_file {
            return Err(PluginError::Runtime(format!(
                "recordArtifact: 产物文件不存在: {file_name:?}"
            )));
        }
        self.db
            .add_task_artifact(task_id, file_name)
            .await
            .map_err(|e| PluginError::Runtime(format!("recordArtifact 落库失败: {e}")))?;
        log_info!(
            "[plugin:{}] recordArtifact: task={} file={}",
            plugin_id,
            task_id,
            file_name
        );
        Ok(())
    }

    async fn ffmpeg_available(&self) -> Option<FfmpegAvailability> {
        let status = crate::components::ffmpeg_status(&self.db, &self.data_dir).await;
        Some(FfmpegAvailability {
            available: !status.path.is_empty(),
            version: status.version,
            source: status.source.as_str().to_string(),
        })
    }

    async fn run_ffmpeg(
        &self,
        _plugin_id: &str,
        jail_root: PathBuf,
        spec: FfmpegSpec,
    ) -> Result<FfmpegOutcome, PluginError> {
        // 生效 ffmpeg（manual→managed→system；不触网）。`-nostdin` 前置。
        let bin = crate::components::resolve_ffmpeg(&self.db, &self.data_dir)
            .await
            .ok_or_else(|| PluginError::Runtime("ffmpeg 未安装或不可用".to_string()))?;
        run_jailed_tool(
            &self.ffmpeg_sema,
            "ffmpeg",
            &bin,
            jail_root,
            spec,
            &["-nostdin"],
        )
        .await
    }

    async fn run_ffprobe(
        &self,
        _plugin_id: &str,
        jail_root: PathBuf,
        spec: FfmpegSpec,
    ) -> Result<FfmpegOutcome, PluginError> {
        // 生效 ffprobe（手动 ffmpeg 同目录 / 托管 / 系统 PATH；随 ffmpeg 组件一并安装）。
        // ffprobe 不识别 `-nostdin`，故无前置（stdin 仍置 null）。与 ffmpeg 同权限门、
        // 同牢笼、同封网/封越牢校验（`validate_ffmpeg_args`）。
        let bin = crate::components::resolve_ffprobe(&self.db, &self.data_dir)
            .await
            .ok_or_else(|| {
                PluginError::Runtime("ffprobe 未安装或不可用（随 ffmpeg 组件一并安装）".to_string())
            })?;
        run_jailed_tool(&self.ffmpeg_sema, "ffprobe", &bin, jail_root, spec, &[]).await
    }

    async fn ytdlp_available(&self) -> Option<YtdlpAvailability> {
        let status = crate::components::ytdlp_status(&self.db, &self.data_dir).await;
        Some(YtdlpAvailability {
            available: !status.path.is_empty(),
            version: status.version,
            source: status.source.as_str().to_string(),
        })
    }

    async fn remove_plugin_workspace(&self, plugin_id: &str) {
        let dir = plugin_workspace(&self.data_dir, plugin_id);
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log_info!("[plugin:{}] 清理工作区失败（已忽略）: {}", plugin_id, e);
        }
    }

    async fn run_ytdlp(
        &self,
        plugin_id: &str,
        spec: YtdlpSpec,
    ) -> Result<YtdlpOutcome, PluginError> {
        // 1) 参数校验（放行 URL；封越牢路径 + 封危险开关）。先于二进制解析 fail-fast。
        if spec.args.is_empty() {
            return Err(PluginError::InvalidOutput(
                "yt-dlp args 不可为空".to_string(),
            ));
        }
        if spec.args.len() > MAX_YTDLP_ARGS {
            return Err(PluginError::InvalidOutput(format!(
                "yt-dlp 参数过多（>{MAX_YTDLP_ARGS}）"
            )));
        }
        validate_ytdlp_args(&spec.args)?;

        // 2) 生效 yt-dlp 二进制（manual→managed→system；不触网）。
        let bin = crate::components::resolve_ytdlp(&self.db, &self.data_dir)
            .await
            .ok_or_else(|| PluginError::Runtime("yt-dlp 未安装或不可用".to_string()))?;

        // yt-dlp 的合并（bestvideo+bestaudio）/抽音（-x）/remux/recode 等后处理依赖
        // ffmpeg。托管 ffmpeg 落在 <data_dir>/bin，不在 PATH，yt-dlp 默认找不到；这里
        // 解析生效 ffmpeg（manual→managed→system）并经 `--ffmpeg-location` 注入。插件
        // 自带的 `--ffmpeg-location` 不在参数白名单内而被拒（防指向任意二进制），宿主注入的
        // 可信路径是唯一来源——两组件由此协同，且不放大攻击面。
        let ffmpeg = crate::components::resolve_ffmpeg(&self.db, &self.data_dir).await;

        // 3) 牢笼根：bridge 自持的每插件工作区（与 flux.fs 同根，懒创建 +
        //    canonicalize）+ 可选安全 subdir，禁逃逸。插件经 flux.fs 物化的输入
        //    文件（cookie/config…）正落在此 cwd，以相对名喂给 yt-dlp。
        let root = plugin_workspace(&self.data_dir, plugin_id);
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|e| PluginError::Runtime(format!("创建 yt-dlp 牢笼失败: {e}")))?;
        let jail = tokio::fs::canonicalize(&root)
            .await
            .map_err(|e| PluginError::Runtime(format!("yt-dlp 牢笼根无效: {e}")))?;
        let work = match spec.subdir.as_deref() {
            Some(sub) if !sub.is_empty() => {
                if !super::manifest::is_safe_relative_path(sub) {
                    return Err(PluginError::InvalidOutput(format!(
                        "yt-dlp subdir '{sub}' 非法"
                    )));
                }
                let cand = jail.join(sub);
                tokio::fs::create_dir_all(&cand)
                    .await
                    .map_err(|e| PluginError::Runtime(format!("创建 yt-dlp subdir 失败: {e}")))?;
                let real = tokio::fs::canonicalize(&cand)
                    .await
                    .map_err(|e| PluginError::Runtime(format!("yt-dlp subdir 无效: {e}")))?;
                if !real.starts_with(&jail) {
                    return Err(PluginError::InvalidOutput(
                        "yt-dlp subdir 逃逸牢笼".to_string(),
                    ));
                }
                real
            }
            _ => jail.clone(),
        };

        // 4) 超时（裁剪到上限）。
        let timeout = spec
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(YTDLP_DEFAULT_TIMEOUT)
            .min(YTDLP_MAX_TIMEOUT);

        // 5) 并发限流。排队等待不计入脚本墙钟（见 `WallPause`），否则批量解析时
        //    后排任务会把排队误判为插件超时并计入熔断。
        let _permit = {
            let _pause = spec.wall_pause.as_ref().map(|p| p.wait());
            self.ytdlp_sema
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| PluginError::Runtime("yt-dlp semaphore closed".to_string()))?
        };

        // 每次调用在启动子进程前快照配置，避免执行期间的配置变更改变返回语义。
        let stdout_cap = ytdlp_stdout_cap(&self.db, plugin_id).await;

        // 6) 启动。`--ignore-config` 前置注入（挡 ambient 配置里的 --exec 等）；
        //    stdin=null；kill_on_drop 保超时/取消时清进程。
        let mut cmd = Command::new(&bin);
        {
            // 首次调用要等登录 shell 给出 PATH：属宿主开销，同排队一样不计入脚本墙钟。
            let _pause = spec.wall_pause.as_ref().map(|p| p.wait());
            apply_full_path_env(&mut cmd).await;
        }
        crate::proc::no_console_window(&mut cmd);
        cmd.current_dir(&work).arg("--ignore-config");
        if let Some(ff) = &ffmpeg {
            cmd.arg("--ffmpeg-location").arg(ff);
        }
        // 缓存收进牢笼（yt-dlp 默认写 ~/.cache/yt-dlp 在牢笼外）。放牢笼根而非
        // subdir，使同插件多次调用共享 nsig/player JS 等缓存；插件自带的
        // `--cache-dir`（只能是牢笼内相对路径）会覆盖此默认，仍在牢笼内。
        cmd.arg("--cache-dir").arg(jail.join(".cache"));
        let (ytdlp_proxy_url, via_fallback) = self
            .net()
            .ytdlp_proxy(spec.egress.as_deref(), &spec.args)
            .await?;
        log_info!(
            "[ytdlp-exec] plugin={} 执行: {} --ignore-config --cache-dir <jail> {}{}",
            plugin_id,
            bin.display(),
            spec.args.join(" "),
            match ytdlp_proxy_url.as_deref() {
                Some("") => " --proxy \"\"",
                Some(_) => " --proxy <redacted>",
                None => "",
            },
        );
        cmd.args(&spec.args);
        // 宿主注入代理（#401）：放在 spec.args 之后——yt-dlp 同名开关后者生效，
        // 用户在 App 里配置的代理对子进程权威；未配置代理（全局 None / Auto 直连路径）
        // 时不注入，插件自带的 `--proxy`（既有插件的自救路径）仍然可用。站点规则 / 任务
        // 显式直连时注入空值，yt-dlp 的 `--proxy ""` 表示强制直连。
        if let Some(proxy_url) = &ytdlp_proxy_url {
            cmd.arg("--proxy").arg(proxy_url);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd
            .spawn()
            .map_err(|e| PluginError::Runtime(format!("启动 yt-dlp 失败: {e}")))?;
        // 观测：yt-dlp 自身的网络失败只在 stderr；未完成（墙钟超时丢弃）同样记入。
        let op = spec
            .egress
            .as_ref()
            .map(|egress| NetObserver::begin(egress, via_fallback));
        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                if let Some(op) = op {
                    op.finish(false);
                }
                return Err(PluginError::Runtime(format!("yt-dlp 执行失败: {e}")));
            }
            Err(_) => {
                // 超时：future 被 drop → kill_on_drop 杀子进程。
                if let Some(op) = op {
                    op.finish(true);
                }
                return Ok(YtdlpOutcome {
                    code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: true,
                    truncated_stdout: false,
                    truncated_stderr: false,
                });
            }
        };
        let (stdout, truncated_stdout) = truncate_utf8(&output.stdout, stdout_cap);
        let (stderr, truncated_stderr) = truncate_utf8(&output.stderr, YTDLP_STDERR_CAP);
        let code = output.status.code().unwrap_or(-1);
        let trunc_mark = if truncated_stdout { " [TRUNCATED]" } else { "" };
        if code == 0 {
            log_info!(
                "[ytdlp-exec] 结果: code=0, stdout={} 字节{}",
                stdout.len(),
                trunc_mark
            );
        } else {
            let stderr_tail: String = {
                let n = stderr.chars().count();
                stderr.chars().skip(n.saturating_sub(600)).collect()
            };
            log_error!(
                "[ytdlp-exec] 结果: code={code}, cwd={}, stdout={} 字节{}, stderr={stderr_tail}",
                work.display(),
                stdout.len(),
                trunc_mark
            );
        }
        if let Some(op) = op {
            op.finish(code != 0 && super::call_egress::ytdlp_transport_failure(&stderr));
        }
        Ok(YtdlpOutcome {
            code: output.status.code().unwrap_or(-1),
            stdout,
            stderr,
            timed_out: false,
            truncated_stdout,
            truncated_stderr,
        })
    }
}

fn reqwest_error_detail(error: &reqwest::Error) -> String {
    let mut detail = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        detail.push_str(": ");
        detail.push_str(&inner.to_string());
        source = inner.source();
    }
    detail
}

/// 在牢笼内执行受管外部工具（ffmpeg / ffprobe）的共用管线：参数校验（封网 +
/// 封越牢）→ 牢笼 canonicalize + 可选 subdir 禁逃逸 → 超时裁剪 → 全局并发限流
/// （`ffmpeg_sema`）→ off-actor 子进程（`prefix` 前置注入、stdin=null、
/// kill_on_drop）→ 输出按上限截断。`tool` 仅用于错误文案。
#[allow(clippy::too_many_arguments)]
async fn run_jailed_tool(
    sema: &Arc<Semaphore>,
    tool: &str,
    bin: &Path,
    jail_root: PathBuf,
    spec: FfmpegSpec,
    prefix: &[&str],
) -> Result<FfmpegOutcome, PluginError> {
    // 1) 参数校验（封网 + 封越牢路径；近乎全量 CLI）。
    if spec.args.is_empty() {
        return Err(PluginError::InvalidOutput(format!("{tool} args 不可为空")));
    }
    if spec.args.len() > MAX_FFMPEG_ARGS {
        return Err(PluginError::InvalidOutput(format!(
            "{tool} 参数过多（>{MAX_FFMPEG_ARGS}）"
        )));
    }
    validate_ffmpeg_args(&spec.args)?;

    // 2) 工作目录：牢笼根（canonicalize）+ 可选安全 subdir，禁逃逸。
    let jail = tokio::fs::canonicalize(&jail_root)
        .await
        .map_err(|e| PluginError::Runtime(format!("{tool} 牢笼根无效: {e}")))?;
    let work = match spec.subdir.as_deref() {
        Some(sub) if !sub.is_empty() => {
            if !super::manifest::is_safe_relative_path(sub) {
                return Err(PluginError::InvalidOutput(format!(
                    "{tool} subdir '{sub}' 非法"
                )));
            }
            let cand = jail.join(sub);
            tokio::fs::create_dir_all(&cand)
                .await
                .map_err(|e| PluginError::Runtime(format!("创建 {tool} subdir 失败: {e}")))?;
            let real = tokio::fs::canonicalize(&cand)
                .await
                .map_err(|e| PluginError::Runtime(format!("{tool} subdir 无效: {e}")))?;
            if !real.starts_with(&jail) {
                return Err(PluginError::InvalidOutput(format!(
                    "{tool} subdir 逃逸牢笼"
                )));
            }
            real
        }
        _ => jail.clone(),
    };

    // 3) 超时（裁剪到上限）。
    let timeout = spec
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(FFMPEG_DEFAULT_TIMEOUT)
        .min(FFMPEG_MAX_TIMEOUT);

    // 4) 并发限流。
    let _permit = sema
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| PluginError::Runtime(format!("{tool} semaphore closed")))?;

    // 5) 启动。`prefix` 前置注入；stdin=null；kill_on_drop 保超时/取消时清进程。
    let mut cmd = Command::new(bin);
    apply_full_path_env(&mut cmd).await;
    crate::proc::no_console_window(&mut cmd);
    cmd.current_dir(&work)
        .args(prefix)
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| PluginError::Runtime(format!("启动 {tool} 失败: {e}")))?;
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(PluginError::Runtime(format!("{tool} 执行失败: {e}"))),
        Err(_) => {
            // 超时：future 被 drop → kill_on_drop 杀子进程。
            return Ok(FfmpegOutcome {
                code: -1,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
                truncated_stdout: false,
                truncated_stderr: false,
            });
        }
    };
    let (stdout, truncated_stdout) = truncate_utf8(&output.stdout, FFMPEG_STDOUT_CAP);
    let (stderr, truncated_stderr) = truncate_utf8(&output.stderr, FFMPEG_STDERR_CAP);
    Ok(FfmpegOutcome {
        code: output.status.code().unwrap_or(-1),
        stdout,
        stderr,
        timed_out: false,
        truncated_stdout,
        truncated_stderr,
    })
}

/// 校验 ffmpeg 参数：封堵网络协议、越牢路径引用，以及能绕过路径检查的二级语法
/// （tee 复用器、带引号/转义的滤镜值、从文件读取选项/列表的开关）；其余
/// （滤镜/编码器/元数据…）近乎全量放行。文件引用一律相对 cwd（牢笼根/subdir）。
fn validate_ffmpeg_args(args: &[String]) -> Result<(), PluginError> {
    for a in args {
        if a.len() > MAX_FFMPEG_ARG_LEN {
            return Err(PluginError::InvalidOutput("ffmpeg 参数过长".to_string()));
        }
        if a.contains('\0') {
            return Err(PluginError::InvalidOutput("ffmpeg 参数含 NUL".to_string()));
        }
        if let Some(reason) = arg_reject_reason(a) {
            return Err(PluginError::InvalidOutput(format!(
                "ffmpeg 参数 '{a}' 被拒: {reason}"
            )));
        }
    }
    Ok(())
}

/// 单参数拒绝原因（`None` = 放行）。判定：绝对路径 / 盘符 / `..` / URL scheme /
/// 协议前缀 / 内嵌绝对路径 / tee 与读文件类开关。除法（`30000/1001`）、流选择器
/// （`0:a`/`-c:v`）、滤镜分隔（`scale=1280:720`）等合法语法均放行。
fn arg_reject_reason(a: &str) -> Option<&'static str> {
    if let Some(r) = ffmpeg_option_reject_reason(a) {
        return Some(r);
    }
    if let Some(r) = ffmpeg_path_reject_reason(a) {
        return Some(r);
    }
    // 滤镜语法允许用 `'`/`"`/`\` 转义包裹值（如 `movie='/abs'`、`C\:/x`），去掉
    // 转义后再判一次，避免引号挡住 `=/` 之类的字面模式。
    if a.contains(['\'', '"', '\\']) {
        let plain: String = a
            .chars()
            .filter(|c| !matches!(c, '\'' | '"' | '\\'))
            .collect();
        if let Some(r) = ffmpeg_path_reject_reason(&plain) {
            return Some(r);
        }
    }
    None
}

/// tee 复用器（`|` 分隔的多输出可写任意路径）、协议白/黑名单覆盖、从文件读取
/// 选项值或 concat 的 `-safe`：这些开关本身即越牢手段，整体拒绝。
fn ffmpeg_option_reject_reason(a: &str) -> Option<&'static str> {
    if a.eq_ignore_ascii_case("tee") {
        return Some("tee 复用器");
    }
    if a.starts_with("-/") {
        return Some("从文件读取选项值");
    }
    let name = a.strip_prefix('-')?;
    let name = name.split(':').next().unwrap_or(name);
    match name {
        "safe" => Some("-safe"),
        "protocol_whitelist" | "protocol_blacklist" => Some("协议白/黑名单覆盖"),
        "filter_script" | "filter_complex_script" => Some("从文件读取滤镜图"),
        _ => None,
    }
}

fn ffmpeg_path_reject_reason(a: &str) -> Option<&'static str> {
    // 绝对路径 / 分隔符开头。
    if a.starts_with('/') || a.starts_with('\\') {
        return Some("绝对路径");
    }
    // Windows 盘符（X: 开头，含 UNC 前缀由上面的 `\` 覆盖）。
    let b = a.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return Some("盘符路径");
    }
    // `..` 路径段（含内嵌 `foo/../bar`）。
    if a.split(['/', '\\']).any(|seg| seg == "..") {
        return Some(".. 越级");
    }
    // 显式 URL（http:// / file:// / rtmp:// …）。
    if a.contains("://") {
        return Some("URL scheme");
    }
    // 无 `//` 的协议前缀（file:/concat:/crypto:/data:/pipe:/subfile: …）：首个 `:`
    // 前缀是 ≥2 位、字母起头的合法 scheme 字符集时判为协议。`-c:v`（`-` 起头）、
    // `0:a`（数字/单字符）、`scale=…:…`（含 `=`）均不满足，放行。
    if let Some(idx) = a.find(':') {
        let scheme = &a[..idx];
        if scheme.len() >= 2
            && scheme.as_bytes()[0].is_ascii_alphabetic()
            && scheme
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'.' || c == b'-')
        {
            return Some("协议前缀");
        }
    }
    // 选项值内嵌的绝对路径（如 `subtitles=/etc/passwd`、`movie=C\:/x`）。
    if a.contains("=/") || a.contains("=\\") || a.contains(":/") || a.contains(":\\") {
        return Some("内嵌绝对路径");
    }
    // 滤镜/多输出语法的分段：任一段以绝对路径或盘符起头（`a.mkv|/abs`、`[v]/abs`）。
    for tok in a.split(['|', ',', ';', '[', ']']) {
        let tb = tok.as_bytes();
        if tok.starts_with('/') || tok.starts_with('\\') {
            return Some("内嵌绝对路径");
        }
        if tb.len() >= 2 && tb[1] == b':' && tb[0].is_ascii_alphabetic() {
            return Some("盘符路径");
        }
    }
    None
}

/// UTF-8 有损转换 + 按字符边界截断到 `cap` 字节。返回 `(文本, 是否截断)`。
fn truncate_utf8(bytes: &[u8], cap: usize) -> (String, bool) {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= cap {
        return (s.into_owned(), false);
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// 解析生效的 yt-dlp stdout 回传上限（字节）。
///
/// 优先级（高→低）：单插件 config `plugin.<id>.ytdlp.stdout_cap` → 全局 config
/// `component.ytdlp.stdout_cap` → 环境变量 `FLUXDOWN_YTDLP_STDOUT_CAP`（仅改默认值）→
/// 常量 [`YTDLP_STDOUT_CAP`]。任何来源都夹到 `[MIN, MAX]`（如 `1g` 按 64 MiB 生效）；
/// 读取 / 解析失败记日志并回退下一级——配置写错不应让下载失败。
async fn ytdlp_stdout_cap(db: &Db, plugin_id: &str) -> usize {
    let per_plugin = format!("plugin.{plugin_id}.ytdlp.stdout_cap");
    for key in [per_plugin.as_str(), CONFIG_YTDLP_STDOUT_CAP] {
        if let Some(cap) = configured_cap(db, key).await {
            return cap;
        }
    }
    std::env::var(ENV_YTDLP_STDOUT_CAP)
        .ok()
        .and_then(|value| parse_cap_source(ENV_YTDLP_STDOUT_CAP, &value))
        .unwrap_or(YTDLP_STDOUT_CAP)
}

/// 环境变量：改 stdout 上限的默认值（字节或带单位，如 `32m`）。
const ENV_YTDLP_STDOUT_CAP: &str = "FLUXDOWN_YTDLP_STDOUT_CAP";

async fn configured_cap(db: &Db, key: &str) -> Option<usize> {
    match db.get_config(key).await {
        Ok(Some(value)) => parse_cap_source(key, &value),
        Ok(None) => None,
        Err(e) => {
            log_warn!("[ytdlp-exec] 读取 {key} 失败，回退下一级: {e:#}");
            None
        }
    }
}

/// 解析并夹取；空值视为未设置，无法解析时记日志返回 `None`（由调用方回退下一级）。
fn parse_cap_source(source: &str, value: &str) -> Option<usize> {
    if value.trim().is_empty() {
        return None;
    }
    let cap = parse_cap(value).map(clamp_cap);
    if cap.is_none() {
        log_warn!("[ytdlp-exec] {source}={value:?} 无法解析为字节数，回退下一级");
    }
    cap
}

/// 解析上限字符串：纯十进制字节，或 `<n>k`/`<n>kb`/`<n>kib`、`<n>m`/`<n>mb`/`<n>mib`、
/// `<n>g`/`<n>gb`/`<n>gib`（大小写不敏感，1024 进制）。其余后缀（如 `16i`、`16b`）判为无效。
fn parse_cap(v: &str) -> Option<usize> {
    let t = v.trim().to_ascii_lowercase();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let mult: usize = match unit.trim() {
        "" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => return None,
    };
    num.parse::<usize>().ok().map(|n| n.saturating_mul(mult))
}

/// 夹到允许区间。
fn clamp_cap(n: usize) -> usize {
    n.clamp(YTDLP_STDOUT_CAP_MIN, YTDLP_STDOUT_CAP_MAX)
}

/// yt-dlp 选项的取值形态。白名单按「是否带值、带几个值」建表，才能分清选项值与
/// 位置参数，并对路径型选项的值做牢笼校验。
#[derive(Clone, Copy, PartialEq, Eq)]
enum YtdlpOpt {
    /// 不带值的开关。
    Flag,
    /// 带一个不含路径语义的值。
    Value,
    /// `--js-runtimes`：只允许裸运行时名（可执行路径由宿主/PATH 负责）。
    JsRuntime,
    /// 带一个牢笼内相对路径。
    Path,
    /// 带一个 `[TYPES:]PATH`（`-o` / `-P`）。
    TypedPath,
    /// `--print-to-file TEMPLATE FILE`（两个值，后者为路径）。
    PrintToFile,
}

/// 仅规范长选项全名（禁缩写）与少数独立出现的短选项。凡会执行外部程序、加载
/// 配置/插件、读浏览器或 netrc 凭据、给 ffmpeg/下载器注入任意参数、别名重定义
/// 选项的开关（`--exec*` / `--netrc*` / `--use-postprocessor` / `--alias` /
/// `--*-args` 的 postprocessor/downloader 变体 / `--config-*` / `--plugin-dirs` /
/// `--load-info-json` / `--batch-file` / `--ffmpeg-location` …）均不在表内。
/// `--ffmpeg-location` 由宿主权威注入。
fn ytdlp_option_kind(name: &str) -> Option<YtdlpOpt> {
    use YtdlpOpt::{Flag, JsRuntime, Path, PrintToFile, TypedPath, Value};
    Some(match name {
        "-J"
        | "-j"
        | "-q"
        | "-v"
        | "-s"
        | "-x"
        | "-F"
        | "-i"
        | "-4"
        | "-6"
        | "-k"
        | "--version"
        | "--dump-json"
        | "--dump-single-json"
        | "--simulate"
        | "--skip-download"
        | "--quiet"
        | "--verbose"
        | "--no-warnings"
        | "--no-progress"
        | "--ignore-errors"
        | "--no-abort-on-error"
        | "--no-playlist"
        | "--yes-playlist"
        | "--flat-playlist"
        | "--no-flat-playlist"
        | "--lazy-playlist"
        | "--playlist-reverse"
        | "--force-ipv4"
        | "--force-ipv6"
        | "--geo-bypass"
        | "--no-geo-bypass"
        | "--no-check-certificates"
        | "--check-formats"
        | "--list-formats"
        | "--live-from-start"
        | "--no-live-from-start"
        | "--no-mtime"
        | "--restrict-filenames"
        | "--windows-filenames"
        | "--no-cache-dir"
        | "--extract-audio"
        | "--keep-video"
        | "--no-keep-video"
        | "--write-info-json"
        | "--write-description"
        | "--write-thumbnail"
        | "--write-subs"
        | "--write-auto-subs"
        | "--no-write-subs"
        | "--embed-subs"
        | "--embed-thumbnail"
        | "--embed-metadata"
        | "--embed-chapters"
        | "--no-part"
        | "--no-continue"
        | "--no-js-runtimes"
        | "--mark-watched"
        | "--no-mark-watched"
        | "--hls-prefer-native"
        | "--hls-use-mpegts"
        | "--no-hls-use-mpegts" => Flag,
        "-f"
        | "-S"
        | "-N"
        | "-r"
        | "-R"
        | "--format"
        | "--format-sort"
        | "--extractor-args"
        | "--playlist-items"
        | "--playlist-start"
        | "--playlist-end"
        | "--max-downloads"
        | "--proxy"
        | "--socket-timeout"
        | "--source-address"
        | "--geo-verification-proxy"
        | "--geo-bypass-country"
        | "--geo-bypass-ip-block"
        | "--xff"
        | "--user-agent"
        | "--referer"
        | "--add-header"
        | "--sleep-interval"
        | "--min-sleep-interval"
        | "--max-sleep-interval"
        | "--sleep-requests"
        | "--sleep-subtitles"
        | "--retries"
        | "--extractor-retries"
        | "--fragment-retries"
        | "--file-access-retries"
        | "--retry-sleep"
        | "--concurrent-fragments"
        | "--limit-rate"
        | "--throttled-rate"
        | "--http-chunk-size"
        | "--buffer-size"
        | "--min-filesize"
        | "--max-filesize"
        | "--date"
        | "--datebefore"
        | "--dateafter"
        | "--match-filters"
        | "--match-filter"
        | "--age-limit"
        | "--download-sections"
        | "--merge-output-format"
        | "--remux-video"
        | "--recode-video"
        | "--audio-format"
        | "--audio-quality"
        | "--sub-langs"
        | "--sub-format"
        | "--convert-subs"
        | "--convert-thumbnails"
        | "--impersonate"
        | "--print"
        | "--parse-metadata"
        | "--compat-options"
        | "--video-password"
        | "--username"
        | "--password"
        | "--twofactor"
        | "--encoding"
        | "--output-na-placeholder" => Value,
        "--js-runtimes" => JsRuntime,
        "--cookies" | "--cache-dir" | "--download-archive" => Path,
        "-o" | "--output" | "-P" | "--paths" => TypedPath,
        "--print-to-file" => PrintToFile,
        _ => return None,
    })
}

/// `-o`/`-P` 值可带的 `TYPE:` 前缀（yt-dlp 输出类型）。
const YTDLP_PATH_TYPES: [&str; 16] = [
    "home",
    "temp",
    "default",
    "subtitle",
    "thumbnail",
    "description",
    "annotation",
    "infojson",
    "link",
    "pl_thumbnail",
    "pl_description",
    "pl_infojson",
    "chapter",
    "pl_video",
    "pl_subtitle",
    "pl_chapter",
];

/// 校验 yt-dlp 参数：选项走白名单（见 [`ytdlp_option_kind`]），位置参数只允许
/// http/https URL，路径型选项的值做牢笼校验。被拒时错误信息写出开关名。
fn validate_ytdlp_args(args: &[String]) -> Result<(), PluginError> {
    for a in args {
        if a.len() > MAX_YTDLP_ARG_LEN {
            return Err(PluginError::InvalidOutput("yt-dlp 参数过长".to_string()));
        }
        if a.contains('\0') {
            return Err(PluginError::InvalidOutput("yt-dlp 参数含 NUL".to_string()));
        }
    }
    match ytdlp_args_reject_reason(args) {
        Some(reason) => Err(PluginError::InvalidOutput(format!(
            "yt-dlp 参数被拒: {reason}"
        ))),
        None => Ok(()),
    }
}

/// 整条参数列表的拒绝原因（`None` = 放行）。
fn ytdlp_args_reject_reason(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    let mut positional_only = false;
    while let Some(a) = it.next() {
        if positional_only || !a.starts_with('-') {
            if let Some(r) = ytdlp_url_reject_reason(a) {
                return Some(format!("位置参数 '{a}': {r}"));
            }
            continue;
        }
        if a == "--" {
            positional_only = true;
            continue;
        }
        // 长选项支持 `--name=value`；短选项只接受独立出现（`-fvalue` / `-Jq` 不在表内）。
        let (name, inline) = if a.starts_with("--") {
            match a.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (a.as_str(), None),
            }
        } else {
            (a.as_str(), None)
        };
        let Some(kind) = ytdlp_option_kind(name) else {
            return Some(format!("开关 '{name}' 不在允许列表内"));
        };
        if kind == YtdlpOpt::Flag {
            if inline.is_some() {
                return Some(format!("开关 '{name}' 不接受值"));
            }
            continue;
        }
        if kind == YtdlpOpt::PrintToFile && inline.is_some() {
            return Some(format!("开关 '{name}' 需以两个独立参数给值"));
        }
        let Some(value) = inline.or_else(|| it.next().map(String::as_str)) else {
            return Some(format!("开关 '{name}' 缺少值"));
        };
        let bad = match kind {
            YtdlpOpt::Flag | YtdlpOpt::Value => None,
            YtdlpOpt::JsRuntime => ytdlp_js_runtime_reject_reason(value),
            YtdlpOpt::Path => ytdlp_path_reject_reason(value),
            YtdlpOpt::TypedPath => ytdlp_path_reject_reason(strip_ytdlp_path_type(value)),
            // 第一个值是输出模板（不落盘），第二个才是目标文件。
            YtdlpOpt::PrintToFile => match it.next() {
                Some(file) => ytdlp_path_reject_reason(file),
                None => Some("缺少文件参数"),
            },
        };
        if let Some(r) = bad {
            return Some(format!("开关 '{name}' 的值被拒: {r}"));
        }
    }
    None
}

/// yt-dlp 参数里的第一个位置参数（待匹配站点规则的 URL）。遍历方式同
/// [`ytdlp_args_reject_reason`]：带值选项的值不算，`--` 之后一律是位置参数。
/// 必须能解析为 http(s) URL，否则 `None`。
fn ytdlp_first_positional_url(args: &[String]) -> Option<url::Url> {
    let mut it = args.iter();
    let mut positional_only = false;
    while let Some(a) = it.next() {
        if positional_only || !a.starts_with('-') {
            let parsed = url::Url::parse(a).ok()?;
            return matches!(parsed.scheme(), "http" | "https").then_some(parsed);
        }
        if a == "--" {
            positional_only = true;
            continue;
        }
        let (name, inline) = if a.starts_with("--") {
            match a.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (a.as_str(), None),
            }
        } else {
            (a.as_str(), None)
        };
        let kind = ytdlp_option_kind(name)?;
        if kind == YtdlpOpt::Flag {
            continue;
        }
        if inline.is_none() {
            it.next();
        }
        if kind == YtdlpOpt::PrintToFile {
            it.next();
        }
    }
    None
}

fn ytdlp_url_reject_reason(a: &str) -> Option<&'static str> {
    let lower = a.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        None
    } else {
        Some("位置参数只允许 http/https URL")
    }
}

fn ytdlp_js_runtime_reject_reason(v: &str) -> Option<&'static str> {
    if !v.is_empty()
        && v.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b',' || c == b'_' || c == b'-')
    {
        None
    } else {
        Some("只允许不带路径的运行时名")
    }
}

fn strip_ytdlp_path_type(v: &str) -> &str {
    match v.split_once(':') {
        Some((ty, rest)) if YTDLP_PATH_TYPES.contains(&ty) => rest,
        _ => v,
    }
}

/// 路径型值拒绝原因：yt-dlp 会对路径做 `expandvars(expanduser())`，所以 `~` /
/// `$VAR` / `%VAR%` 与绝对路径、盘符、`..` 一样能逃出牢笼。`%(title)s` 这类输出
/// 模板字段不是环境变量形态，放行。
fn ytdlp_path_reject_reason(v: &str) -> Option<&'static str> {
    if v.starts_with('~') {
        return Some("~ 家目录展开");
    }
    if v.contains('$') {
        return Some("$ 变量展开");
    }
    if has_percent_env_var(v) {
        return Some("%VAR% 变量展开");
    }
    if v.starts_with('/') || v.starts_with('\\') {
        return Some("绝对路径");
    }
    let b = v.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Some("盘符路径");
    }
    if v.split(['/', '\\']).any(|seg| seg == "..") {
        return Some(".. 越级");
    }
    if v.contains(":/") || v.contains(":\\") {
        return Some("内嵌绝对路径");
    }
    None
}

/// 是否含 `%NAME%` 形态（NAME 为 `[A-Za-z_][A-Za-z0-9_]*`）。
fn has_percent_env_var(v: &str) -> bool {
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 1 < b.len() && (b[i + 1].is_ascii_alphabetic() || b[i + 1] == b'_') {
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j < b.len() && b[j] == b'%' {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{
        GuardError, YTDLP_STDOUT_CAP_MAX, YTDLP_STDOUT_CAP_MIN, arg_reject_reason, clamp_cap,
        collect_response_headers, guard_filter, is_globally_routable_unicast,
        normalize_explicit_auth_ref, parse_cap, plugin_workspace, truncate_utf8,
        validate_ffmpeg_args, validate_ytdlp_args, ytdlp_args_reject_reason, ytdlp_stdout_cap,
    };
    use std::net::{IpAddr, SocketAddr};

    fn sa(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse::<IpAddr>().unwrap(), 0)
    }

    #[test]
    fn guard_filter_passes_all_fake_ip_through_for_tun() {
        let resolved = vec![sa("198.18.0.42"), sa("198.19.255.1")];
        assert_eq!(guard_filter(resolved.clone()).ok(), Some(resolved));
    }

    #[test]
    fn guard_filter_keeps_only_public_in_mixed_answer() {
        let resolved = vec![sa("198.18.0.42"), sa("142.250.80.46"), sa("10.0.0.1")];
        assert_eq!(guard_filter(resolved).ok(), Some(vec![sa("142.250.80.46")]));
    }

    #[test]
    fn guard_filter_blocks_internal_only_answers() {
        for set in [
            vec![sa("10.0.0.1")],
            vec![sa("127.0.0.1"), sa("192.168.1.5")],
            // fake-ip 与内网混合不构成 TUN 应答，仍拦截
            vec![sa("198.18.0.1"), sa("10.0.0.1")],
            vec![sa("::1")],
        ] {
            assert!(matches!(guard_filter(set), Err(GuardError::BlockedHost)));
        }
    }

    #[test]
    fn guard_filter_empty_answer_stays_empty() {
        assert_eq!(guard_filter(Vec::new()).ok(), Some(Vec::new()));
    }

    #[test]
    fn guard_filter_treats_ipv4_mapped_fake_ip_as_fake_ip() {
        // sing-box inet6_range 等给出的 IPv4-mapped 应答与 v4 fake-ip 混合：整体视为 TUN 应答
        let resolved = vec![sa("198.18.0.42"), sa("::ffff:198.19.0.7")];
        assert_eq!(guard_filter(resolved.clone()).ok(), Some(resolved));
        // mapped 的内网地址不是 fake-ip，仍拦截
        assert!(matches!(
            guard_filter(vec![sa("::ffff:198.18.0.1"), sa("::ffff:10.0.0.1")]),
            Err(GuardError::BlockedHost)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_write_protects_new_and_existing_sensitive_files()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::PluginBridge;
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("fluxdown-private-fs-{}", uuid::Uuid::new_v4()));
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let (retry_tx, _retry_rx) = tokio::sync::mpsc::unbounded_channel();
        let bridge = super::EngineBridge::new(
            db,
            &crate::proxy_config::ProxyConfig::default(),
            retry_tx,
            root.clone(),
        )?;
        bridge
            .fs_write(
                "acme@private",
                "cookies.txt",
                "old-secret-and-long-tail".into(),
            )
            .await?;
        let path = plugin_workspace(&root, "acme@private").join("cookies.txt");
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        bridge
            .fs_write("acme@private", "cookies.txt", "new".into())
            .await?;
        assert_eq!(std::fs::read(&path)?, b"new");
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    /// 起一个只应答一次的本地 HTTP 服务，返回其端口。
    fn serve_once() -> Result<u16, Box<dyn std::error::Error>> {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                if stream.read(&mut buf).is_ok()
                    && stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .is_err()
                {
                    eprintln!("serve_once: write failed");
                }
            }
        });
        Ok(port)
    }

    async fn test_bridge() -> Result<super::EngineBridge, Box<dyn std::error::Error>> {
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        let (retry_tx, _retry_rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(super::EngineBridge::new(
            db,
            &crate::proxy_config::ProxyConfig::default(),
            retry_tx,
            std::env::temp_dir(),
        )?)
    }

    #[tokio::test]
    async fn fetch_loopback_blocked_for_regular_plugin() -> Result<(), Box<dyn std::error::Error>> {
        use super::PluginBridge;
        let bridge = test_bridge().await?;
        let req = crate::plugin::runtime::BridgeHttpRequest {
            url: "http://127.0.0.1:9/".to_string(),
            ..Default::default()
        };
        let err = bridge
            .http_request("acme@regular", req)
            .await
            .expect_err("loopback must be blocked");
        assert!(err.to_string().contains("blocked: non-routable IP"));
        Ok(())
    }

    #[tokio::test]
    async fn fetch_loopback_allowed_for_dev_plugin() -> Result<(), Box<dyn std::error::Error>> {
        use super::PluginBridge;
        let port = serve_once()?;
        let bridge = test_bridge().await?;
        let req = crate::plugin::runtime::BridgeHttpRequest {
            url: format!("http://127.0.0.1:{port}/"),
            private_network_allowed: true,
            ..Default::default()
        };
        let resp = bridge.http_request("acme@dev", req).await?;
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "ok");
        Ok(())
    }

    /// 回归（Auto 换路绕开 SSRF 守卫）：主机名只解析到内网地址被守卫拦下，不能记成断网——
    /// 否则 Auto 会改经代理重跑，由代理侧解析把请求送进内网。
    #[tokio::test]
    async fn guard_blocked_hostname_is_not_a_transport_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::PluginBridge;
        let bridge = test_bridge().await?;
        let egress = std::sync::Arc::new(super::CallEgress::new(
            crate::proxy_config::ProxyConfig::default(),
            false,
            None,
        ));
        let req = crate::plugin::runtime::BridgeHttpRequest {
            url: "http://localhost:9/".to_string(),
            egress: Some(std::sync::Arc::clone(&egress)),
            ..Default::default()
        };
        let err = bridge
            .http_request("acme@regular", req)
            .await
            .expect_err("localhost must be blocked for regular plugins");
        assert!(err.to_string().contains("non-routable"), "{err}");
        assert!(
            !egress.observer().transport_suspect(&err, true),
            "guard block must not trigger an Auto proxy retry"
        );
        Ok(())
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap_or_else(|_| panic!("bad ip {s}"))
    }

    #[test]
    fn empty_explicit_auth_ref_normalizes_to_none() {
        assert_eq!(normalize_explicit_auth_ref(Some(String::new())), None);
        assert_eq!(normalize_explicit_auth_ref(None), None);
        assert_eq!(
            normalize_explicit_auth_ref(Some("a@b::https://x.com".to_string())),
            Some("a@b::https://x.com".to_string())
        );
    }

    #[test]
    fn ffmpeg_args_accept_legit_syntax() {
        // 常见合法参数：滤镜/编码器/流选择器/除法/时间戳/相对文件名，均须放行。
        for a in [
            "-i",
            "video.ts",
            "-c",
            "copy",
            "out.mp4",
            "-vf",
            "scale=1280:720",
            "-r",
            "30000/1001",
            "-map",
            "0:a",
            "-c:v",
            "libx264",
            "-b:v",
            "2M",
            "-ss",
            "00:01:02",
            "-metadata:s:a:0",
            "title=x",
            "-vf",
            "setpts=0.5*PTS",
            "-filter_complex",
            "overlay=W-w:H-h",
            "sub.srt",
            "clip.audio.m4a",
            "-y",
        ] {
            assert!(
                arg_reject_reason(a).is_none(),
                "'{a}' should be accepted, got {:?}",
                arg_reject_reason(a)
            );
        }
    }

    #[test]
    fn ffmpeg_args_reject_network_and_escape() {
        // 越牢路径 + 网络协议，须逐一拒绝。
        for a in [
            "/etc/passwd",
            "\\\\server\\share",
            "C:\\Windows\\system32",
            "../secret",
            "a/../../b",
            "http://evil.example/x",
            "https://evil/x",
            "file:/etc/passwd",
            "concat:a.ts|b.ts",
            "crypto:key",
            "subfile:,start,0,end,10,:in",
            "subtitles=/etc/passwd",
            "movie=C\\:/secret",
        ] {
            assert!(arg_reject_reason(a).is_some(), "'{a}' should be rejected");
        }
    }

    #[test]
    fn ffmpeg_validate_rejects_nul_and_reports() {
        assert!(validate_ffmpeg_args(&["ok.mp4".into()]).is_ok());
        assert!(validate_ffmpeg_args(&["bad\0name".into()]).is_err());
        assert!(validate_ffmpeg_args(&["/abs".into()]).is_err());
    }

    fn ytdlp_args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ytdlp_args_accept_example_plugin_and_relative_paths() {
        // examples/plugins/ytdlp 实际使用的参数集 + 相对输出模板/路径类型前缀。
        for set in [
            vec![
                "-J",
                "--no-warnings",
                "--extractor-args",
                "youtube:player_client=default,web_safari",
                "-f",
                "bestvideo+bestaudio/best",
                "--playlist-items",
                "2",
                "--js-runtimes",
                "node",
                "--cookies",
                "cookies.txt",
                "--parse-metadata",
                "automatic_captions:(?P<automatic_captions>)",
                "--parse-metadata",
                "heatmap:(?P<heatmap>)",
                "--parse-metadata",
                "subtitles:(?P<subtitles>)",
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            ],
            vec![
                "--no-playlist",
                "--proxy=http://127.0.0.1:7890",
                "http://example.com/a?x=1&y=2",
            ],
            vec!["-o", "%(title)s.%(ext)s", "--paths", "temp:sub"],
            vec![
                "--add-header",
                "Referer:https://site.example/",
                "--merge-output-format=mp4",
            ],
            vec!["--download-sections", "*00:01:00-00:02:00", "--version"],
        ] {
            assert_eq!(ytdlp_args_reject_reason(&ytdlp_args(&set)), None, "{set:?}");
        }
    }

    #[test]
    fn ytdlp_args_reject_non_whitelisted_switch_forms() {
        // 类别：危险开关全名 / 长选项缩写 / 合并或紧贴值的短选项 / 别名 / 执行类。
        for set in [
            vec!["--exec", "x"],
            vec!["--exec=x"],
            vec!["--exec-before-download", "x"],
            vec!["--config-loc", "x"],
            vec!["--config-locations", "x"],
            vec!["--plugin-d", "x"],
            vec!["--external-down", "x"],
            vec!["--batch-f", "x"],
            vec!["-a", "x"],
            vec!["-afile"],
            vec!["-Jq"],
            vec!["-fbest"],
            vec!["--alias", "x"],
            vec!["--netrc-cmd", "x"],
            vec!["--use-postprocessor", "x"],
            vec!["--postprocessor-args", "x"],
            vec!["--ppa", "x"],
            vec!["--ffmpeg-location", "x"],
            vec!["--cookies-from-browser", "x"],
            vec!["--load-info-json", "x"],
            vec!["--no-warn"],
        ] {
            assert!(
                ytdlp_args_reject_reason(&ytdlp_args(&set)).is_some(),
                "{set:?} should be rejected"
            );
        }
    }

    #[test]
    fn ytdlp_args_reject_arity_and_positional_violations() {
        for set in [
            vec!["-J=1"],
            vec!["-f"],
            vec!["--print-to-file", "%(id)s"],
            vec!["/etc/passwd"],
            vec!["file:///etc/passwd"],
            vec!["ftp://example.com/x"],
            vec!["--js-runtimes", "node:relative/bin"],
            vec!["--", "rel.txt"],
        ] {
            assert!(
                ytdlp_args_reject_reason(&ytdlp_args(&set)).is_some(),
                "{set:?} should be rejected"
            );
        }
    }

    #[test]
    fn ytdlp_args_jail_path_option_values() {
        // 路径型选项的值：绝对 / 盘符 / `..` / 家目录 / 变量展开，以及把 `://` 夹在
        // 路径里伪装 URL，一律拒绝；输出模板字段 `%(x)s` 不误伤。
        for (opt, v) in [
            ("-o", "/abs/%(title)s"),
            ("-o", "~/x"),
            ("-o", "$HOME/x"),
            ("-o", "%APPDATA%\\x"),
            ("-o", "../x"),
            ("-o", "C:\\x"),
            ("-o", "/abs://x"),
            ("-P", "home:/abs"),
            ("-P", "temp:~/x"),
            ("--paths", "thumbnail:..\\x"),
            ("--cookies", "/etc/x"),
            ("--download-archive", "$X/a"),
            ("--cache-dir", "~"),
        ] {
            assert!(
                ytdlp_args_reject_reason(&ytdlp_args(&[opt, v])).is_some(),
                "{opt} {v} should be rejected"
            );
        }
        assert!(
            ytdlp_args_reject_reason(&ytdlp_args(&["--print-to-file", "%(id)s", "/abs/x"]))
                .is_some()
        );
        assert_eq!(
            ytdlp_args_reject_reason(&ytdlp_args(&["-o", "%(title)s [%(id)s].%(ext)s"])),
            None
        );
        assert_eq!(
            ytdlp_args_reject_reason(&ytdlp_args(&["--print-to-file", "%(id)s", "ids.txt"])),
            None
        );
    }

    #[test]
    fn ytdlp_rejection_names_the_switch() {
        let err = validate_ytdlp_args(&["--exec".into(), "x".into()])
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("--exec"), "{err}");
    }

    #[test]
    fn ytdlp_validate_rejects_nul_and_reports() {
        assert!(validate_ytdlp_args(&["-J".into(), "https://x/y".into()]).is_ok());
        assert!(validate_ytdlp_args(&["bad\0name".into()]).is_err());
        assert!(validate_ytdlp_args(&["--exec".into()]).is_err());
    }

    #[test]
    fn ffmpeg_args_reject_tee_and_quoted_filter_paths() {
        for a in [
            "tee",
            "out.mkv|/abs/x",
            "[f=mp4]/abs/x",
            "movie='/abs/x'",
            "movie=C\\:/x",
            "subtitles=\"/abs/x\"",
            "-safe",
            "-protocol_whitelist",
            "-filter_script:v",
            "-filter_complex_script",
            "-/vf",
        ] {
            assert!(arg_reject_reason(a).is_some(), "'{a}' should be rejected");
        }
        // 相对路径的合法滤镜（含引号转义）不误伤。
        assert!(arg_reject_reason("subtitles='sub.srt'").is_none());
        assert!(arg_reject_reason("drawtext=text='hi there':x=10").is_none());
        assert!(arg_reject_reason("join=map=0.0-FL|1.0-FR").is_none());
    }

    #[test]
    fn plugin_workspace_is_collision_free() {
        let root = std::path::Path::new("/d");
        assert_ne!(
            plugin_workspace(root, "a_b@c"),
            plugin_workspace(root, "a@b_c")
        );
        assert_ne!(
            plugin_workspace(root, "a_40b"),
            plugin_workspace(root, "a@b")
        );
        let p = plugin_workspace(root, "../x@y");
        assert_eq!(p.parent(), Some(root.join("plugins-work").as_path()));
    }

    #[test]
    fn truncate_utf8_respects_char_boundary() {
        let (s, t) = truncate_utf8("hello".as_bytes(), 100);
        assert_eq!(s, "hello");
        assert!(!t);
        // 3 字节字符边界：cap=4 落在第二个 '啊'(3 字节) 中间 → 回退到 3。
        let (s, t) = truncate_utf8("啊啊".as_bytes(), 4);
        assert_eq!(s, "啊");
        assert!(t);
    }

    #[test]
    fn parse_cap_accepts_bytes_and_suffixes_and_clamps() {
        assert_eq!(parse_cap("4194304"), Some(4 * 1024 * 1024));
        assert_eq!(parse_cap("16m"), Some(16 * 1024 * 1024));
        assert_eq!(parse_cap("16mb"), Some(16 * 1024 * 1024));
        assert_eq!(parse_cap("16MiB"), Some(16 * 1024 * 1024));
        assert_eq!(parse_cap("512k"), Some(512 * 1024));
        assert_eq!(parse_cap("512kb"), Some(512 * 1024));
        assert_eq!(parse_cap("512kib"), Some(512 * 1024));
        assert_eq!(parse_cap("1g"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_cap("1gb"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_cap("1gib"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_cap("  8M  "), Some(8 * 1024 * 1024));
        assert_eq!(parse_cap("1.5m"), None);
        assert_eq!(parse_cap("-1"), None);
        assert_eq!(parse_cap("m"), None);
        assert_eq!(parse_cap("bogus"), None);
        assert_eq!(parse_cap(""), None);
        // 不完整 / 非法单位一律无效，回退下一级而不是被当成字节数夹到下限。
        for bad in ["16i", "16b", "16bi", "16mi", "16kbb", "16 x"] {
            assert_eq!(parse_cap(bad), None, "{bad}");
        }
        assert_eq!(parse_cap("16 m"), Some(16 * 1024 * 1024));
        assert_eq!(parse_cap(&usize::MAX.to_string()), Some(usize::MAX));
        assert_eq!(parse_cap(&format!("{}m", usize::MAX)), Some(usize::MAX));
        assert_eq!(parse_cap(&format!("{}0", usize::MAX)), None);
        // 夹到允许区间。
        assert_eq!(clamp_cap(0), YTDLP_STDOUT_CAP_MIN);
        assert_eq!(clamp_cap(usize::MAX), YTDLP_STDOUT_CAP_MAX);
        assert_eq!(clamp_cap(16 * 1024 * 1024), 16 * 1024 * 1024);
    }

    #[tokio::test]
    async fn invalid_plugin_cap_falls_back_to_global() -> Result<(), Box<dyn std::error::Error>> {
        let db = crate::db::Db::connect("sqlite::memory:").await?;
        db.set_config("plugin.test@ytdlp.ytdlp.stdout_cap", "16i")
            .await?;
        db.set_config(super::CONFIG_YTDLP_STDOUT_CAP, "2m").await?;

        assert_eq!(ytdlp_stdout_cap(&db, "test@ytdlp").await, 2 * 1024 * 1024);
        // 超出区间的合法值按上限生效。
        db.set_config("plugin.test@ytdlp.ytdlp.stdout_cap", "1g")
            .await?;
        assert_eq!(
            ytdlp_stdout_cap(&db, "test@ytdlp").await,
            YTDLP_STDOUT_CAP_MAX
        );
        Ok(())
    }

    #[test]
    fn response_headers_join_duplicate_values_in_wire_order() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append(
            reqwest::header::HeaderName::from_static("set-cookie"),
            reqwest::header::HeaderValue::from_static("a=1"),
        );
        headers.append(
            reqwest::header::HeaderName::from_static("set-cookie"),
            reqwest::header::HeaderValue::from_static("b=2"),
        );
        let result = collect_response_headers(&headers);
        assert_eq!(
            result.get("set-cookie").map(String::as_str),
            Some("a=1\nb=2")
        );
    }

    #[test]
    fn v4_rejects_private_and_special() {
        for s in [
            "127.0.0.1",       // loopback
            "10.0.0.1",        // private
            "172.16.0.1",      // private
            "192.168.1.1",     // private
            "169.254.169.254", // link-local (cloud metadata)
            "100.64.0.1",      // CGNAT
            "198.18.0.1",      // benchmarking
            "240.0.0.1",       // reserved
            "0.0.0.0",         // this-network
            "255.255.255.255", // broadcast
            "224.0.0.1",       // multicast
            "192.0.2.1",       // documentation
        ] {
            assert!(
                !is_globally_routable_unicast(ip(s)),
                "{s} should be rejected"
            );
        }
    }

    #[test]
    fn v4_allows_public() {
        for s in ["8.8.8.8", "1.1.1.1", "93.184.216.34"] {
            assert!(is_globally_routable_unicast(ip(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn v6_rejects_special() {
        for s in [
            "::1",                // loopback
            "fe80::1",            // link-local
            "fc00::1",            // ULA
            "fd00::1",            // ULA
            "2001:db8::1",        // documentation
            "2001:0::1",          // Teredo
            "ff02::1",            // multicast
            "::",                 // unspecified
            "::ffff:127.0.0.1",   // v4-mapped loopback
            "2002:0a00:0001::",   // 6to4 embedding 10.0.0.1
            "64:ff9b::0a00:0001", // NAT64 embedding 10.0.0.1
        ] {
            assert!(
                !is_globally_routable_unicast(ip(s)),
                "{s} should be rejected"
            );
        }
    }

    #[test]
    fn v6_allows_public() {
        for s in ["2606:4700:4700::1111", "2001:4860:4860::8888"] {
            assert!(is_globally_routable_unicast(ip(s)), "{s} should be allowed");
        }
    }
}

#[cfg(test)]
mod rule_tests {
    use std::sync::Arc;

    use super::{BridgeNet, ytdlp_first_positional_url};
    use crate::plugin::call_egress::CallEgress;
    use crate::proxy_config::{ProxyConfig, ProxyMode, ProxyType};
    use crate::proxy_rules::ProxyRuleSet;

    const RULES: &str = r#"[
        {"id":"a","match":"domain","pattern":"example.com","action":"custom","proxy":"http://127.0.0.1:9"},
        {"id":"b","match":"domain","pattern":"direct.test","action":"direct"}
    ]"#;

    fn rules() -> Arc<ProxyRuleSet> {
        Arc::new(ProxyRuleSet::compile(RULES, &ProxyConfig::default()))
    }

    fn net() -> BridgeNet {
        BridgeNet {
            global: ProxyConfig {
                rules: Some(rules()),
                ..ProxyConfig::default()
            },
        }
    }

    fn manual(port: u16) -> ProxyConfig {
        ProxyConfig {
            mode: ProxyMode::Manual,
            proxy_type: ProxyType::Http,
            host: "127.0.0.1".to_string(),
            port,
            ..ProxyConfig::default()
        }
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn first_positional_skips_option_values() {
        let url = ytdlp_first_positional_url(&args(&[
            "--referer",
            "https://referer.test/",
            "https://example.com/v",
        ]));
        assert_eq!(
            url.map(|u| u.host_str().map(str::to_owned)),
            Some(Some("example.com".to_string()))
        );
        let inline = ytdlp_first_positional_url(&args(&[
            "--referer=https://referer.test/",
            "-J",
            "https://example.com/v",
        ]));
        assert!(inline.is_some_and(|u| u.host_str() == Some("example.com")));
    }

    #[test]
    fn first_positional_after_double_dash_and_invalid() {
        let url = ytdlp_first_positional_url(&args(&["--", "https://example.com/x"]));
        assert!(url.is_some());
        assert!(ytdlp_first_positional_url(&args(&["-J"])).is_none());
        assert!(ytdlp_first_positional_url(&args(&["ftp://example.com/x"])).is_none());
        assert!(ytdlp_first_positional_url(&args(&["not a url"])).is_none());
    }

    #[tokio::test]
    async fn ytdlp_proxy_follows_first_positional_rule() -> Result<(), Box<dyn std::error::Error>> {
        let net = net();
        let (hit, via_fallback) = net
            .ytdlp_proxy(
                None,
                &args(&["--referer", "https://direct.test/", "https://example.com/v"]),
            )
            .await?;
        assert!(hit.is_some_and(|p| p.starts_with("http://127.0.0.1:9")));
        assert!(!via_fallback);
        let (direct, _) = net
            .ytdlp_proxy(None, &args(&["https://x.direct.test/v"]))
            .await?;
        assert_eq!(direct, Some(String::new()));
        let (miss, via_fallback) = net
            .ytdlp_proxy(
                None,
                &args(&["--referer", "https://example.com/", "https://other.test/v"]),
            )
            .await?;
        assert_eq!(
            miss, None,
            "全局未配置代理：不注入，插件自带 --proxy 仍可用"
        );
        assert!(via_fallback);
        Ok(())
    }

    #[tokio::test]
    async fn call_egress_replaces_global_fallback_but_rules_still_win()
    -> Result<(), Box<dyn std::error::Error>> {
        let net = net();
        let auto_proxy = CallEgress::new(manual(7890), true, Some(rules()));
        let (miss, via_fallback) = net
            .ytdlp_proxy(Some(&auto_proxy), &args(&["https://other.test/v"]))
            .await?;
        assert_eq!(miss.as_deref(), Some("http://127.0.0.1:7890"));
        assert!(via_fallback);
        let (hit, via_fallback) = net
            .ytdlp_proxy(Some(&auto_proxy), &args(&["https://example.com/v"]))
            .await?;
        assert!(hit.is_some_and(|p| p.starts_with("http://127.0.0.1:9")));
        assert!(!via_fallback, "规则命中的请求不归因到调用出口");

        let auto_direct = CallEgress::new(ProxyConfig::default(), false, Some(rules()));
        let (none, _) = net
            .ytdlp_proxy(Some(&auto_direct), &args(&["https://other.test/v"]))
            .await?;
        assert_eq!(none, None);
        let explicit_direct = CallEgress::new(ProxyConfig::default(), true, None);
        let (forced, _) = net
            .ytdlp_proxy(Some(&explicit_direct), &args(&["https://example.com/v"]))
            .await?;
        assert_eq!(
            forced,
            Some(String::new()),
            "任务自带直连：不看规则且强制直连"
        );

        let url = url::Url::parse("https://other.test/a")?;
        let route = net.route(Some(&auto_proxy), &url, false).await?;
        assert_eq!(route.proxy.port, 7890);
        assert!(route.via_fallback);
        let url = url::Url::parse("https://www.example.com/a")?;
        let route = net.route(Some(&auto_proxy), &url, false).await?;
        assert_eq!(route.proxy.port, 9);
        assert!(!route.via_fallback);
        Ok(())
    }
}
