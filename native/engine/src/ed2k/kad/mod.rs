//! eMule Kad（Kademlia DHT）去中心化找源。
//!
//! 服务器全挂时的兜底找源路径：解析 `nodes.dat` 引导联系点 → 迭代
//! `FindNode` 逼近 `target = file_hash` 的 KadID → 对最近节点发
//! `SearchSourcesReq` → 提取源。协议编解码见 [`proto`]，UDP 节点与迭代查找
//! 见 [`node`]。
//!
//! 引导联系点来自用户订阅的一个或多个 `nodes.dat` 地址（`ed2k_nodes_dat_url` 配置，
//! 每行一个）：[`fetch_nodes_subscriptions`] 逐个拉取、合并去重后重新编码为一份
//! `nodes.dat`，由宿主写入缓存键 `ed2k_nodes_dat_cache`。

pub mod node;
pub mod proto;

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use crate::ed2k::status::KadNodeRow;
use crate::logger::{log_error, log_info};

/// nodes.dat 下载大小上限（正常几十~几百 KB；超此视为异常）。
const MAX_NODES_DAT_BYTES: usize = 4 * 1024 * 1024;
/// nodes.dat 下载超时。
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// 合并后缓存的引导联系点上限（一轮找源只向离目标最近的几十个发包，更多没有收益）。
const MAX_BOOTSTRAP_CONTACTS: usize = 2000;
/// 引导握手的收包窗口。
const BOOTSTRAP_WINDOW: Duration = Duration::from_secs(4);
/// 解析手动指定节点域名的超时。
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

/// 拉取所有 nodes.dat 订阅源的结果。
#[derive(Debug)]
pub struct NodesFetchOutcome {
    /// 所有响应成功的源合并去重后的联系点，编码为一份 `nodes.dat`；全部失败时为空。
    pub nodes_dat: Vec<u8>,
    /// `nodes_dat` 里的联系点数。
    pub node_count: usize,
    /// 成功拉取的源数。
    pub ok_sources: usize,
    /// 尝试的订阅源总数。
    pub total_sources: usize,
    /// 仅当**全部**源失败时非空（拼接的错误摘要）。
    pub error: String,
}

impl NodesFetchOutcome {
    /// 至少一个源成功即为成功。
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.ok_sources > 0
    }

    fn failed(total_sources: usize, error: String) -> Self {
        Self {
            nodes_dat: Vec::new(),
            node_count: 0,
            ok_sources: 0,
            total_sources,
            error,
        }
    }
}

/// 把订阅配置拆成地址列表：每行一个，跳过空行与 `#` 注释，解开
/// `ed2k://|nodeslist|<url>|/` 链接壳。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::ed2k::kad::subscription_urls;
/// assert_eq!(
///     subscription_urls("# kad\nhttps://a.example/nodes.dat\n\ned2k://|nodeslist|http://b.example/n.dat|/"),
///     ["https://a.example/nodes.dat", "http://b.example/n.dat"]
/// );
/// ```
#[must_use]
pub fn subscription_urls(urls: &str) -> Vec<String> {
    urls.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| crate::ed2k::server_subscription::unwrap_list_link(line, "nodeslist"))
        .filter(|url| !url.is_empty())
        .collect()
}

/// 合并多个源的联系点：按 `(IP, UDP 端口)` 保序去重，丢弃无法发包的条目
/// （端口 0 / 未指定地址），至多保留 [`MAX_BOOTSTRAP_CONTACTS`] 个。
fn merge_contacts<I>(sources: I) -> Vec<proto::Contact>
where
    I: IntoIterator<Item = proto::Contact>,
{
    let mut seen: HashSet<(Ipv4Addr, u16)> = HashSet::new();
    let mut merged = Vec::new();
    for contact in sources {
        let endpoint = contact.endpoint;
        if endpoint.udp_port == 0 || endpoint.ip.is_unspecified() {
            continue;
        }
        if seen.insert((endpoint.ip, endpoint.udp_port)) {
            merged.push(contact);
            if merged.len() >= MAX_BOOTSTRAP_CONTACTS {
                break;
            }
        }
    }
    merged
}

/// 拉取每个订阅地址（见 [`subscription_urls`]）并返回合并去重后的引导联系点。
/// 请求经宿主统一出口（[`crate::egress::RoutedClients`]），与下载任务同一套全局代理与站点规则。
///
/// 单源失败可容忍：只要至少一源给出可解析的联系点即为成功。所有网络错误都记日志。
///
/// # Examples
///
/// ```no_run
/// # async fn run() {
/// use fluxdown_engine::ed2k::kad::fetch_nodes_subscriptions;
/// use fluxdown_engine::proxy_config::ProxyConfig;
/// let outcome = fetch_nodes_subscriptions(
///     "https://upd.emule-security.org/nodes.dat",
///     &ProxyConfig::default(),
/// )
/// .await;
/// # let _ = outcome.is_success();
/// # }
/// ```
pub async fn fetch_nodes_subscriptions(
    urls: &str,
    proxy: &crate::proxy_config::ProxyConfig,
) -> NodesFetchOutcome {
    let sources = subscription_urls(urls);
    let total_sources = sources.len();
    if total_sources == 0 {
        return NodesFetchOutcome::failed(0, "no subscription URLs configured".to_string());
    }
    let clients = match crate::egress::RoutedClients::new(proxy, "", |b| b.timeout(FETCH_TIMEOUT)) {
        Ok(clients) => clients,
        Err(error) => {
            return NodesFetchOutcome::failed(
                total_sources,
                format!("failed to build http client: {error}"),
            );
        }
    };

    let mut contacts: Vec<proto::Contact> = Vec::new();
    let mut ok_sources = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for url in &sources {
        match fetch_one(&clients, url).await {
            Ok(found) => {
                log_info!(
                    "[ed2k-nodes-sub] fetched {} contacts from {}",
                    found.len(),
                    url
                );
                ok_sources += 1;
                contacts.extend(found);
            }
            Err(error) => {
                log_error!("[ed2k-nodes-sub] fetch failed: {}: {}", url, error);
                errors.push(format!("{url}: {error}"));
            }
        }
    }

    let merged = merge_contacts(contacts);
    log_info!(
        "[ed2k-nodes-sub] refresh done: {}/{} sources ok, {} unique contacts",
        ok_sources,
        total_sources,
        merged.len()
    );
    NodesFetchOutcome {
        nodes_dat: proto::encode_nodes_dat(&merged),
        node_count: merged.len(),
        ok_sources,
        total_sources,
        error: if ok_sources == 0 {
            errors.join("; ")
        } else {
            String::new()
        },
    }
}

/// 拉取单个订阅源并解析出联系点；解析不出任何联系点视为失败。
async fn fetch_one(
    clients: &crate::egress::RoutedClients,
    url: &str,
) -> Result<Vec<proto::Contact>, String> {
    let client = clients.client(url).await.map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("http status {status}"));
    }
    let body = resp.bytes().await.map_err(|e| e.to_string())?;
    if body.len() > MAX_NODES_DAT_BYTES {
        return Err(format!("nodes.dat too large ({} bytes)", body.len()));
    }
    let contacts = proto::parse_nodes_dat(&body);
    if contacts.is_empty() {
        return Err("no contacts parsed (not a nodes.dat?)".to_string());
    }
    Ok(contacts)
}

/// Kad 引导的节点来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapSource {
    /// 已知节点（nodes.dat 缓存）。
    Known,
    /// 先从这个地址取 nodes.dat；空串 = 用已配置的订阅地址。
    Url(String),
    /// 用户指定的单个节点：IP 或域名 + Kad UDP 端口。
    Node { host: String, port: u16 },
}

/// [`bootstrap`] 的结果。
#[derive(Debug, Clone, Default)]
pub struct BootstrapReport {
    /// 发过握手请求的节点数。
    pub contacted: u32,
    /// 应答的节点数；0 = Kad 网络当前不可达（或指定的节点已下线）。
    pub responded: u32,
    /// 应答里新告知的节点数。
    pub discovered: u32,
    /// 本次联系与发现的节点（应答的在前）。
    pub nodes: Vec<KadNodeRow>,
    /// 可并入已知节点缓存的联系点，越可信的越靠前：应答的节点、它们告知的节点、
    /// 以及（来源是网址时）从网址取到的全部节点。
    pub contacts: Vec<proto::Contact>,
    /// 没有可联系的节点时的原因；其余情况为空。
    pub error: String,
}

impl BootstrapReport {
    fn failed(error: String) -> Self {
        Self {
            error,
            ..Self::default()
        }
    }
}

/// 把 `host:port` 解析为一个可发包的 IPv4 联系点（KadID 未知，等它应答时自报）。
async fn resolve_node(host: &str, port: u16) -> Result<proto::Contact, String> {
    let host = host.trim();
    if host.is_empty() || port == 0 {
        return Err("node address and UDP port are required".to_string());
    }
    let resolved = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| format!("resolving {host} timed out"))?
        .map_err(|error| format!("cannot resolve {host}: {error}"))?;
    let ip = resolved
        .filter_map(|addr| match addr {
            SocketAddr::V4(v4) => Some(*v4.ip()),
            SocketAddr::V6(_) => None,
        })
        .find(|ip| !ip.is_unspecified())
        .ok_or_else(|| format!("{host} has no IPv4 address (Kad is IPv4 only)"))?;
    Ok(proto::Contact {
        id: proto::KadId::UNKNOWN,
        endpoint: proto::Endpoint {
            ip,
            udp_port: port,
            tcp_port: 0,
        },
        version: 0,
    })
}

/// Kad 引导：按 `source` 取得要联系的节点，向它们发握手并报告应答情况。用于确认 Kad 网络
/// 可达并取得鲜活节点，不查找任何文件。会连接网络（取 nodes.dat、解析域名、UDP 握手）。
///
/// - `cached_nodes_dat`：已知节点缓存（[`BootstrapSource::Known`] 用）。
/// - `subscription_urls`：已配置的订阅地址（[`BootstrapSource::Url`] 给空串时用）。
/// - `tcp_port`：握手里上报的本机 eD2K TCP 端口。
pub async fn bootstrap(
    source: BootstrapSource,
    cached_nodes_dat: &[u8],
    subscription_urls: &str,
    tcp_port: u16,
    proxy: &crate::proxy_config::ProxyConfig,
) -> BootstrapReport {
    // `fetched`：从网址取到的节点，即使没有一个应答也值得留作已知节点。
    let (targets, fetched) = match source {
        BootstrapSource::Known => {
            let known = proto::parse_nodes_dat(cached_nodes_dat);
            if known.is_empty() {
                return BootstrapReport::failed("no known Kad nodes yet".to_string());
            }
            (known, Vec::new())
        }
        BootstrapSource::Url(url) => {
            let urls = if url.trim().is_empty() {
                subscription_urls
            } else {
                url.as_str()
            };
            let outcome = fetch_nodes_subscriptions(urls, proxy).await;
            if !outcome.is_success() {
                return BootstrapReport::failed(outcome.error);
            }
            let fetched = proto::parse_nodes_dat(&outcome.nodes_dat);
            (fetched.clone(), fetched)
        }
        BootstrapSource::Node { host, port } => match resolve_node(&host, port).await {
            Ok(contact) => (vec![contact], Vec::new()),
            Err(error) => return BootstrapReport::failed(error),
        },
    };
    match node::bootstrap(&targets, tcp_port, BOOTSTRAP_WINDOW).await {
        Ok(outcome) => {
            let mut contacts = outcome.live;
            contacts.extend(fetched);
            BootstrapReport {
                contacted: outcome.contacted,
                responded: outcome.responded,
                discovered: outcome.discovered,
                nodes: outcome.nodes,
                contacts,
                error: String::new(),
            }
        }
        Err(error) => BootstrapReport {
            contacts: fetched,
            error: error.to_string(),
            ..BootstrapReport::default()
        },
    }
}

/// 把新取得的联系点并入已知节点缓存：新的在前（缓存满时先淘汰旧的），去重规则同订阅合并。
/// 返回新的缓存（`nodes.dat` 编码）与其中的节点数。
#[must_use]
pub fn merge_into_cache(fresh: Vec<proto::Contact>, cached_nodes_dat: &[u8]) -> (Vec<u8>, usize) {
    let merged = merge_contacts(
        fresh
            .into_iter()
            .chain(proto::parse_nodes_dat(cached_nodes_dat)),
    );
    (proto::encode_nodes_dat(&merged), merged.len())
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::proto::{Contact, Endpoint, KadId, encode_nodes_dat, parse_nodes_dat};
    use super::{
        BootstrapSource, MAX_BOOTSTRAP_CONTACTS, bootstrap, merge_contacts, merge_into_cache,
        subscription_urls,
    };

    fn contact(last: u8, udp_port: u16) -> Contact {
        Contact {
            id: KadId([last; 16]),
            endpoint: Endpoint {
                ip: Ipv4Addr::new(10, 0, 0, last),
                udp_port,
                tcp_port: 4662,
            },
            version: 9,
        }
    }

    #[test]
    fn merged_sources_round_trip_through_the_cache_encoding() {
        // 两个源有一个重复节点，另有不可达条目（端口 0 / 0.0.0.0）。
        let first = vec![contact(1, 4672), contact(2, 4672), contact(3, 0)];
        let mut unspecified = contact(4, 4672);
        unspecified.endpoint.ip = Ipv4Addr::UNSPECIFIED;
        let second = vec![contact(2, 4672), unspecified, contact(5, 4672)];

        let merged = merge_contacts(first.into_iter().chain(second));
        assert_eq!(
            merged,
            [contact(1, 4672), contact(2, 4672), contact(5, 4672)]
        );
        // 同一 IP 的另一个 UDP 端口是另一个节点。
        assert_eq!(
            merge_contacts([contact(1, 4672), contact(1, 4673)]).len(),
            2
        );
        assert_eq!(parse_nodes_dat(&encode_nodes_dat(&merged)), merged);
    }

    #[test]
    fn empty_merge_encodes_to_an_empty_cache() {
        let bytes = encode_nodes_dat(&[]);
        assert!(bytes.is_empty());
        assert!(parse_nodes_dat(&bytes).is_empty());
    }

    #[test]
    fn merge_caps_the_cached_contact_count() {
        let many = (0..MAX_BOOTSTRAP_CONTACTS + 50).map(|index| {
            let mut entry = contact(1, 4672);
            entry.endpoint.ip = Ipv4Addr::from(0x0A00_0000 + index as u32 + 1);
            entry
        });
        assert_eq!(merge_contacts(many).len(), MAX_BOOTSTRAP_CONTACTS);
    }

    #[test]
    fn fresh_contacts_lead_the_merged_cache() {
        let cached = encode_nodes_dat(&[contact(1, 4672), contact(2, 4672)]);
        let (bytes, count) = merge_into_cache(vec![contact(3, 4672), contact(2, 4672)], &cached);
        assert_eq!(count, 3);
        assert_eq!(
            parse_nodes_dat(&bytes),
            [contact(3, 4672), contact(2, 4672), contact(1, 4672)]
        );
        // 缓存损坏 / 为空不妨碍并入。
        assert_eq!(merge_into_cache(vec![contact(3, 4672)], &[1, 2, 3]).1, 1);
        assert_eq!(merge_into_cache(Vec::new(), &[]).1, 0);
    }

    #[tokio::test]
    async fn bootstrap_reports_why_there_is_nothing_to_contact() {
        let proxy = crate::proxy_config::ProxyConfig::default();
        let known = bootstrap(BootstrapSource::Known, &[], "", 4662, &proxy).await;
        assert!(!known.error.is_empty());
        assert_eq!(known.contacted, 0);

        let blank_host = BootstrapSource::Node {
            host: "  ".to_owned(),
            port: 4672,
        };
        assert!(
            !bootstrap(blank_host, &[], "", 4662, &proxy)
                .await
                .error
                .is_empty()
        );
        let no_port = BootstrapSource::Node {
            host: "127.0.0.1".to_owned(),
            port: 0,
        };
        assert!(
            !bootstrap(no_port, &[], "", 4662, &proxy)
                .await
                .error
                .is_empty()
        );

        // 没给网址、也没有配置订阅地址：不触网直接报告。
        let no_url = bootstrap(
            BootstrapSource::Url(String::new()),
            &[],
            "# none",
            4662,
            &proxy,
        );
        assert!(!no_url.await.error.is_empty());
    }

    #[test]
    fn subscription_urls_skip_comments_and_unwrap_link_shells() {
        assert_eq!(
            subscription_urls(
                "  https://a.example/nodes.dat  \n\n# note\nED2K://|nodeslist|http://b.example/N.dat|/\ned2k://|nodeslist||/"
            ),
            ["https://a.example/nodes.dat", "http://b.example/N.dat"]
        );
        assert!(subscription_urls("").is_empty());
    }
}
