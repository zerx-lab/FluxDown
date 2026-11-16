//! `ProxyMode::Auto`——「直连起飞，多路径同窗调度」的候选与上下文层。
//!
//! # 设计
//!
//! Auto 模式不再有「采样 → 判定 → 一次性热切换」的决策机器。直连、手动
//! 代理、系统代理（以及直连下的多 CDN 钉定节点）被统一建模为同一个
//! [`crate::cdn::NodePool`] 里的**路径**：coordinator 每个 ramp 窗口按连接
//! 实测稳态速率（剔除建连首窗、限速窗、段尾窗）更新各路径估计，
//! 租借按「竞争集内择优 + 分散」派工，慢路径按 ECF（Earliest Completion
//! First）判据拿不到会拖慢整体完成的工作，在途慢连接按完成时间判据被
//! 抢占并在当前字节处交接（详见 [`crate::path_scheduler`]）。「采样」即
//! 真实分段下载，字节全部写入文件，零丢弃。
//!
//! 本模块只负责：候选代理解析（手动字段 + 系统代理，同端点去重）、任务级
//! 多路径上下文 [`AutoProxyCtx`]、路由 wire 标签与 failover 错误分类。
//! 跨任务先验由 [`crate::route_health`] 以折扣估计持久化。
//!
//! # 完整性
//!
//! 代理路径上的每个分段与直连分段走完全相同的 `do_segment` 校验（206、
//! `Content-Range` 起点/总长、ETag/Last-Modified 与跨段 validator latch），
//! 校验先于首字节写盘；代理路径出错按节点可归因语义回收重派，validator
//! 不一致即踢除该路径并记 NoSwitch，绝不污染 host 级缓存。
//!
//! # 路由可追溯
//!
//! 每个任务的主导链路（累计字节最多的路径）以 wire 标签写入
//! `tasks.auto_route` 并广播 [`crate::events::EngineEvent::TaskRouteChanged`]，
//! 标签见 [`route`] 常量组。

use crate::proxy_config::{ProxyConfig, ProxyMode};

// ---------------------------------------------------------------------------
// 路由标签（wire 契约：DB `tasks.auto_route`、TaskRouteChanged 事件、
// api TaskDto.autoRoute 与 Web WS 逐字一致）
// ---------------------------------------------------------------------------

/// `tasks.auto_route` 的 wire 标签（任务详情「链路」行的事实源）。空串 = 任务尚未
/// 启动过，或出口 client 构建失败退回全局直连（不对路由做声明）。
///
/// 代理类标签带候选来源后缀 `:system`（系统代理检测）/ `:manual`（手动
/// 字段回退），UI 通用解析后缀展示「最终用的是谁」；无后缀的裸标签是
/// 旧库存量值，语义不变。非 Auto 的 `global:*` / `task:*` / `bt:*` 标签只做展示与
/// 日志，不参与 Auto 换路（[`route_of_label`](super::route_of_label) 对它们返回 `None`）。
pub mod route {
    use super::CandidateSource;

    /// Auto 直连（默认路径，未探索任何备选路径）。
    pub const DIRECT: &str = "direct";
    /// 备选代理路径已被实测，直连仍是主导路径。
    pub const DIRECT_SAMPLED: &str = "direct:sampled";
    /// 代理路径 validator 不一致被踢除（完整性优先），直连主导。
    pub const DIRECT_PINNED: &str = "direct:pinned";
    /// 代理失败后自动回退直连。
    pub const DIRECT_FAILOVER: &str = "direct:failover";
    /// 运行中实测使代理路径成为主导路径。
    pub const PROXY_SAMPLED: &str = "proxy:sampled";
    /// 起飞时按跨任务先验直接走代理。
    pub const PROXY_CACHED: &str = "proxy:cached";
    /// 直连失败后经代理自动换路重试。
    pub const PROXY_FAILOVER: &str = "proxy:failover";
    /// 带来源后缀的代理标签（base × {system,manual}，全部静态）。
    pub const PROXY_SAMPLED_SYSTEM: &str = "proxy:sampled:system";
    pub const PROXY_SAMPLED_MANUAL: &str = "proxy:sampled:manual";
    pub const PROXY_CACHED_SYSTEM: &str = "proxy:cached:system";
    pub const PROXY_CACHED_MANUAL: &str = "proxy:cached:manual";
    pub const PROXY_FAILOVER_SYSTEM: &str = "proxy:failover:system";
    pub const PROXY_FAILOVER_MANUAL: &str = "proxy:failover:manual";
    /// 站点规则决定任务出口，实际直连（`direct` 动作、`system` 未检测到代理、
    /// `manual` 未配置）。
    pub const RULE_DIRECT: &str = "rule:direct";
    /// 站点规则决定任务出口，经系统代理。
    pub const RULE_SYSTEM: &str = "rule:system";
    /// 站点规则决定任务出口，经手动代理。
    pub const RULE_MANUAL: &str = "rule:manual";
    /// 站点规则决定任务出口，经规则自带的自定义代理。
    pub const RULE_CUSTOM: &str = "rule:custom";
    /// 全局「不使用代理」，或手动 / 系统模式下没有可用代理（手动字段不全、系统未
    /// 检测到代理、系统 PAC 结论 DIRECT）。
    pub const GLOBAL_DIRECT: &str = "global:direct";
    /// 全局代理生效，但本链接命中直连名单（用户例外列表 / 系统例外 / 环回）。
    pub const GLOBAL_BYPASS: &str = "global:bypass";
    /// 全局手动代理。
    pub const GLOBAL_MANUAL: &str = "global:manual";
    /// 全局系统代理（静态代理）。
    pub const GLOBAL_SYSTEM: &str = "global:system";
    /// 全局系统代理模式下按系统 PAC 脚本选中的代理。
    pub const GLOBAL_PAC: &str = "global:pac";
    /// 任务级专属代理（任务属性里填写的代理地址）。
    pub const TASK_PROXY: &str = "task:proxy";
    /// 任务级代理实际直连（`direct://`，或 `system://` 但未检测到系统代理）。
    pub const TASK_DIRECT: &str = "task:direct";
    /// BT 会话直连（未开启 BT 代理，或开启了但没有可用的代理服务器）。
    pub const BT_DIRECT: &str = "bt:direct";
    /// BT 仅 tracker 与 HTTP 源走代理（对端 / DHT 直连），带来源后缀。
    pub const BT_TRACKERS_MANUAL: &str = "bt:trackers:manual";
    pub const BT_TRACKERS_SYSTEM: &str = "bt:trackers:system";
    /// BT 全部流量走代理（含对端连接与 DHT），带来源后缀。
    pub const BT_ALL_MANUAL: &str = "bt:all:manual";
    pub const BT_ALL_SYSTEM: &str = "bt:all:system";

    /// 是否为站点规则标签（Auto 故障转移等 Auto 专属逻辑必须跳过这类任务）。
    pub fn is_rule_label(label: &str) -> bool {
        label.starts_with("rule:")
    }

    /// 代理基础标签 + 候选来源 → 带后缀标签（非代理基础标签原样返回）。
    pub fn with_source(base: &'static str, source: CandidateSource) -> &'static str {
        match (base, source) {
            (PROXY_SAMPLED, CandidateSource::System) => PROXY_SAMPLED_SYSTEM,
            (PROXY_SAMPLED, CandidateSource::ManualFields) => PROXY_SAMPLED_MANUAL,
            (PROXY_CACHED, CandidateSource::System) => PROXY_CACHED_SYSTEM,
            (PROXY_CACHED, CandidateSource::ManualFields) => PROXY_CACHED_MANUAL,
            (PROXY_FAILOVER, CandidateSource::System) => PROXY_FAILOVER_SYSTEM,
            (PROXY_FAILOVER, CandidateSource::ManualFields) => PROXY_FAILOVER_MANUAL,
            _ => base,
        }
    }

    /// 任意代理标签 → failover 变体（保留来源后缀；非代理标签原样返回）。
    pub fn to_failover(label: &'static str) -> &'static str {
        if !label.starts_with("proxy") {
            return label;
        }
        if label.ends_with(":system") {
            PROXY_FAILOVER_SYSTEM
        } else if label.ends_with(":manual") {
            PROXY_FAILOVER_MANUAL
        } else {
            PROXY_FAILOVER
        }
    }
}

/// Auto 候选代理的来源（决定 wire 标签后缀，供 UI 展示「最终用的是谁」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateSource {
    /// 系统代理检测命中（Windows 注册表等）。
    System,
    /// 用户在设置中填写的手动代理地址。
    ManualFields,
}

/// 任务的一条下载路径（多路径池内每个槽位归属其一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoutePath {
    /// 直连（SYS 槽位或多 CDN 钉定节点）。
    Direct,
    /// 经指定来源的候选代理。
    Proxy(CandidateSource),
    /// 出口绑定到指定网卡（OS 接口索引）的直连额外链路（多网卡聚合，见
    /// [`crate::multi_nic`]）。与其它路径容量独立，派工按独立容量均衡。
    Link(u32),
}

impl RoutePath {
    /// 路径作为「主导链路」时的采样标签（运行中由实测转为主导）。网卡链路
    /// 本质是直连，与 Auto 多路径互斥（manager 保证），按直连归类。
    #[must_use]
    pub fn sampled_label(self) -> &'static str {
        match self {
            Self::Direct | Self::Link(_) => route::DIRECT_SAMPLED,
            Self::Proxy(source) => route::with_source(route::PROXY_SAMPLED, source),
        }
    }

    /// 是否多网卡聚合的额外链路。
    #[must_use]
    pub fn is_link(self) -> bool {
        matches!(self, Self::Link(_))
    }
}

// ---------------------------------------------------------------------------
// 候选代理解析
// ---------------------------------------------------------------------------

/// 已解析为可直接构建 client 的 Auto 代理候选。
#[derive(Debug, Clone)]
pub struct ProxyCandidate {
    pub config: ProxyConfig,
    pub source: CandidateSource,
}

/// 解析 Auto 模式的全部候选代理：手动字段与系统代理同时存在时全部返回；
/// 完全相同的代理 URL 只保留手动候选（显式配置优先，避免重复采样同一端点）。
///
/// 仅对 `mode == Auto` 的配置有意义；其余模式返回空列表。
pub fn resolve_candidates(config: &ProxyConfig) -> Vec<ProxyCandidate> {
    if config.mode != ProxyMode::Auto {
        return Vec::new();
    }
    resolve_candidates_with_system(config, crate::proxy_config::detect_auto_system_proxy())
}

fn resolve_candidates_with_system(
    config: &ProxyConfig,
    system: Option<ProxyConfig>,
) -> Vec<ProxyCandidate> {
    if config.mode != ProxyMode::Auto {
        return Vec::new();
    }

    let mut candidates = Vec::with_capacity(2);
    if !config.host.is_empty() && config.port != 0 {
        let mut manual = config.clone();
        manual.mode = ProxyMode::Manual;
        // 派生出口配置不携带站点规则（规则只挂在全局配置上）。
        manual.rules = None;
        candidates.push(ProxyCandidate {
            config: manual,
            source: CandidateSource::ManualFields,
        });
    }
    if let Some(system) = system {
        let system_url = system.to_proxy_url();
        let duplicate = candidates
            .iter()
            .any(|candidate| candidate.config.to_proxy_url() == system_url);
        if !duplicate {
            candidates.push(ProxyCandidate {
                config: system,
                source: CandidateSource::System,
            });
        }
    }
    candidates
}

// ---------------------------------------------------------------------------
// 任务级多路径上下文（manager 构造，穿透 DownloadParams 进 coordinator）
// ---------------------------------------------------------------------------

/// 起飞路径之外的一条备选路径（coordinator 为其构建 client 并挂入节点池）。
#[derive(Debug, Clone)]
pub struct PathCandidate {
    pub route: RoutePath,
    /// 该路径的 client 配置（直连 = `ProxyConfig::default()`）。
    pub config: ProxyConfig,
    /// 跨任务折扣先验：单连接稳态速率（B/s）。`None` = 冷路径，需探索。
    pub prior_bps: Option<f64>,
}

/// Auto 任务的多路径上下文。仅当存在至少一条备选路径且任务未忽略 TLS
/// 错误时构造；其余情况为 `None`（单路径，行为与非 Auto 一致）。
#[derive(Debug, Clone)]
pub struct AutoProxyCtx {
    /// 任务 URL 的 host（含端口），先验与 NoSwitch 记录的 key。
    pub host: String,
    /// 任务解析后的有效 UA（任务 > 队列 > 全局），备选路径 client 与
    /// 任务 client 保持一致。
    pub user_agent: String,
    /// 任务 client（节点池 SYS 槽位）所走的路径。
    pub start_route: RoutePath,
    /// 起飞路径的跨任务先验（B/s）。`None` = 保持池默认初值。
    pub start_prior_bps: Option<f64>,
    /// manager 写入的起飞标签。主导路径仍是起飞路径时 coordinator 不覆盖
    /// failover/cached 这类语义更具体的标签。
    pub start_label: &'static str,
    /// 起飞路径之外的全部备选路径。
    pub alternates: Vec<PathCandidate>,
}

// ---------------------------------------------------------------------------
// failover 支路（manager 侧调用）
// ---------------------------------------------------------------------------

/// 去掉 reqwest 错误文本里的 `for url (…)`：URL 里恰好含 `eof` / `dns` /
/// `timeout` 之类子串时，永久性 HTTP 错误（404/403）不应被当成传输错误。
fn strip_url_context(lower: &str) -> String {
    let mut out = String::with_capacity(lower.len());
    let mut rest = lower;
    while let Some(pos) = rest.find("for url (") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + "for url (".len()..];
        // URL 不含空白；其后的 `)` 及错误链从第一个空白起继续保留。
        rest = match after.find(char::is_whitespace) {
            Some(i) => &after[i..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// 路由切换可能修复的传输层错误。HTTP 下载器写入的文案带类型化分类标签
/// （[`crate::net_failure::tag_of`]），有标签即以标签为准；无标签的文案
/// （FTP / HLS / DASH 下载器、旧库存量）回退英文关键词。HTTP 状态、校验
/// 失败等永久错误不应靠换链路重试。
pub fn is_route_transport_error(msg: &str) -> bool {
    if crate::net_failure::tag_of(msg).is_some() {
        return true;
    }
    let lower = strip_url_context(&msg.to_lowercase());
    let lower = lower.as_str();
    lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("network unreachable")
        || lower.contains("network is down")
        || lower.contains("no route to host")
        || lower.contains("dns")
        || lower.contains("stalled")
        || lower.contains("broken pipe")
        || lower.contains("eof")
        || lower.contains("connection closed")
        || lower.contains("connection abort")
        || lower.contains("incomplete download")
        || lower.contains("error decoding response body")
}

/// 路由标签 → 路径。裸 `proxy:*`（旧库存量，无来源后缀）无法确定是哪条
/// 代理 → `None`；空标签与非 Auto 标签（`global:*` / `task:*` / `bt:*` / `rule:*`）→ `None`。
#[must_use]
pub fn route_of_label(label: &str) -> Option<RoutePath> {
    if label.starts_with("direct") {
        Some(RoutePath::Direct)
    } else if !label.starts_with("proxy") {
        None
    } else if label.ends_with(":system") {
        Some(RoutePath::Proxy(CandidateSource::System))
    } else if label.ends_with(":manual") {
        Some(RoutePath::Proxy(CandidateSource::ManualFields))
    } else {
        None
    }
}

/// 非 Auto、未命中站点规则的 HTTP 族任务的出口标签。
///
/// `global_mode` 是全局代理模式，`task_proxy_url` 是任务级代理（非空即优先），
/// `resolved` 是实际用来构建任务 client 的出口配置（已 [`ProxyConfig::resolve`]），
/// `url` 是任务链接（判定直连名单）。Auto 模式由多路径决策给标签，这里返回空串。
#[must_use]
pub(crate) fn static_route_label(
    global_mode: &ProxyMode,
    task_proxy_url: &str,
    resolved: &ProxyConfig,
    url: &str,
) -> &'static str {
    let proxied = resolved.mode != ProxyMode::None && resolved.to_proxy_url().is_some();
    if !task_proxy_url.is_empty() {
        return if proxied {
            route::TASK_PROXY
        } else {
            route::TASK_DIRECT
        };
    }
    let via = match global_mode {
        ProxyMode::Auto => return "",
        ProxyMode::None => return route::GLOBAL_DIRECT,
        ProxyMode::Manual => route::GLOBAL_MANUAL,
        ProxyMode::System => route::GLOBAL_SYSTEM,
    };
    if !proxied {
        return route::GLOBAL_DIRECT;
    }
    let bypassed = url::Url::parse(url.trim()).is_ok_and(|parsed| {
        parsed.host_str().is_some_and(|host| {
            crate::proxy_config::host_matches_no_proxy(
                host,
                &crate::proxy_config::proxy_bypass_list(&resolved.no_proxy_list),
            )
        })
    });
    if bypassed { route::GLOBAL_BYPASS } else { via }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::proxy_config::ProxyType;

    fn auto_config(host: &str, port: u16) -> ProxyConfig {
        ProxyConfig {
            mode: ProxyMode::Auto,
            proxy_type: ProxyType::Http,
            host: host.to_string(),
            port,
            username: String::new(),
            password: String::new(),
            no_proxy_list: String::new(),
            rules: None,
        }
    }

    #[test]
    fn static_route_label_reports_actual_egress() {
        let url = "http://tlu.dl.delivery.mp.microsoft.com/files/a.cab";
        let mut manual = auto_config("10.0.0.2", 7897);
        manual.mode = ProxyMode::Manual;
        let direct = ProxyConfig::default();

        assert_eq!(
            static_route_label(&ProxyMode::None, "", &direct, url),
            route::GLOBAL_DIRECT
        );
        assert_eq!(
            static_route_label(&ProxyMode::Manual, "", &manual, url),
            route::GLOBAL_MANUAL
        );
        // 系统代理解析后是 Manual 配置；标签按全局模式区分来源。
        assert_eq!(
            static_route_label(&ProxyMode::System, "", &manual, url),
            route::GLOBAL_SYSTEM
        );
        // 系统模式未检测到代理 / 手动字段不全：实际直连。
        assert_eq!(
            static_route_label(&ProxyMode::System, "", &direct, url),
            route::GLOBAL_DIRECT
        );
        let incomplete = ProxyConfig {
            host: String::new(),
            ..manual.clone()
        };
        assert_eq!(
            static_route_label(&ProxyMode::Manual, "", &incomplete, url),
            route::GLOBAL_DIRECT
        );
        // 直连名单命中。
        let bypass = ProxyConfig {
            no_proxy_list: ".microsoft.com".into(),
            ..manual.clone()
        };
        assert_eq!(
            static_route_label(&ProxyMode::Manual, "", &bypass, url),
            route::GLOBAL_BYPASS
        );
        // 任务级代理优先于全局模式。
        assert_eq!(
            static_route_label(&ProxyMode::None, "socks5://10.0.0.3:1080", &manual, url),
            route::TASK_PROXY
        );
        assert_eq!(
            static_route_label(&ProxyMode::Manual, "system://", &direct, url),
            route::TASK_DIRECT
        );
        // Auto 由多路径决策给标签。
        assert_eq!(static_route_label(&ProxyMode::Auto, "", &direct, url), "");
        // 非 Auto 标签不参与 Auto 换路。
        for label in [
            route::GLOBAL_MANUAL,
            route::GLOBAL_SYSTEM,
            route::BT_ALL_SYSTEM,
        ] {
            assert_eq!(route_of_label(label), None, "{label}");
        }
    }

    #[test]
    fn resolve_candidates_ignores_non_auto_modes() {
        let mut cfg = auto_config("127.0.0.1", 7890);
        let mut system = auto_config("127.0.0.1", 7891);
        system.mode = ProxyMode::Manual;
        cfg.mode = ProxyMode::Manual;
        assert!(resolve_candidates_with_system(&cfg, Some(system.clone())).is_empty());
        cfg.mode = ProxyMode::None;
        assert!(resolve_candidates_with_system(&cfg, Some(system)).is_empty());
    }

    #[test]
    fn resolve_candidates_keeps_manual_and_distinct_system_proxy() {
        let cfg = auto_config("127.0.0.1", 7890);
        let mut system = auto_config("127.0.0.1", 7891);
        system.mode = ProxyMode::Manual;

        let candidates = resolve_candidates_with_system(&cfg, Some(system));

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].source, CandidateSource::ManualFields);
        assert_eq!(candidates[0].config.port, 7890);
        assert_eq!(candidates[1].source, CandidateSource::System);
        assert_eq!(candidates[1].config.port, 7891);
    }

    #[test]
    fn resolve_candidates_deduplicates_identical_proxy_endpoint() {
        let cfg = auto_config("127.0.0.1", 7890);
        let mut system = cfg.clone();
        system.mode = ProxyMode::Manual;

        let candidates = resolve_candidates_with_system(&cfg, Some(system));

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, CandidateSource::ManualFields);
    }

    #[test]
    fn resolve_candidates_accepts_system_without_manual_fields() {
        let cfg = auto_config("", 0);
        let mut system = auto_config("127.0.0.1", 7891);
        system.mode = ProxyMode::Manual;

        let candidates = resolve_candidates_with_system(&cfg, Some(system));

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source, CandidateSource::System);
    }

    // ---- 路由标签组合 --------------------------------------------------------

    #[test]
    fn route_source_suffix_composition() {
        use route::*;
        assert_eq!(
            with_source(PROXY_SAMPLED, CandidateSource::System),
            PROXY_SAMPLED_SYSTEM
        );
        assert_eq!(
            with_source(PROXY_CACHED, CandidateSource::ManualFields),
            PROXY_CACHED_MANUAL
        );
        // 非代理基础标签不带后缀。
        assert_eq!(with_source(DIRECT, CandidateSource::System), DIRECT);
        assert_eq!(DIRECT_FAILOVER, "direct:failover");
        // failover 变体保留来源后缀；裸标签（旧库存量）不造假来源。
        assert_eq!(to_failover(PROXY_CACHED_SYSTEM), PROXY_FAILOVER_SYSTEM);
        assert_eq!(to_failover(PROXY_SAMPLED_MANUAL), PROXY_FAILOVER_MANUAL);
        assert_eq!(to_failover(PROXY_CACHED), PROXY_FAILOVER);
        assert_eq!(to_failover(DIRECT), DIRECT);
    }

    // ---- failover 错误分类 ---------------------------------------------------

    #[test]
    fn route_transport_errors_include_connect_and_midstream_failures() {
        assert!(is_route_transport_error(
            "Connection refused (os error 111)"
        ));
        assert!(is_route_transport_error("operation timed out"));
        assert!(is_route_transport_error("dns error: no record"));
        assert!(is_route_transport_error("download stalled for 5s"));
        assert!(is_route_transport_error("unexpected EOF"));
        assert!(is_route_transport_error("error decoding response body"));
        assert!(!is_route_transport_error("HTTP 404 Not Found"));
        assert!(!is_route_transport_error("checksum mismatch"));
        // URL 里的 eof/dns/timeout 子串不是传输错误证据。
        assert!(!is_route_transport_error(
            "HTTP status client error (404 Not Found) for url (https://dl.example/eof/dns-timeout.bin)"
        ));
        assert!(is_route_transport_error(
            "error sending request for url (https://dl.example/a.bin): connection refused"
        ));
    }
}
