//! 发现层：mDNS 局域网自动发现（广播 + 浏览）+ 手动地址 `/ping` 探测。
//!
//! # 两个职责（设计文档 §6.5）
//! 1. **发现设备以加入名册**（免账号本地配对入口）：浏览 `_fluxdown._tcp.local.`。
//! 2. **为已知设备找最快连接路径**：mDNS 得到的 `ip:port` 即 Direct 候选。
//!
//! **扩展点**：发现方式是可替换策略；未来可加其他发现源（如账户名册回填、二维码
//! 带地址），只要往 [`DiscoveredPeer`] 汇流即可，配对/传输层无感。mDNS 广播/浏览
//! 各自独立运行在 mdns-sd 自建线程上（不阻塞宿主的 async runtime）。

use std::net::{IpAddr, UdpSocket};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

use super::error::{LinkError, LinkResult};
use super::types::{DiscoveredPeer, DiscoveryKind};

/// FluxDown 局域网服务类型（DNS-SD）。
pub const SERVICE_TYPE: &str = "_fluxdown._tcp.local.";

/// TXT 记录键。
const TXT_FINGERPRINT: &str = "fp";
const TXT_NAME: &str = "name";
const TXT_PLATFORM: &str = "plat";
const TXT_VERSION: &str = "ver";

fn map_mdns_err(e: mdns_sd::Error) -> LinkError {
    LinkError::Io(e.to_string())
}

/// mDNS 广播器：向局域网通告本设备的 fluxdown 服务（供其他设备发现并配对）。
/// 持有 daemon 句柄，`Drop` 时优雅关闭。
pub struct MdnsAdvertiser {
    daemon: ServiceDaemon,
}

impl MdnsAdvertiser {
    /// 开始广播。`port` 为本机 fluxdown API 端口；TXT 携带身份指纹/名称/平台/版本。
    pub fn start(
        port: u16,
        fingerprint: &str,
        name: &str,
        platform: Option<&str>,
        app_version: Option<&str>,
    ) -> LinkResult<Self> {
        let daemon = ServiceDaemon::new().map_err(map_mdns_err)?;
        // 实例名用短指纹保证唯一（同名设备不冲突）；host_name 走 <fp>.local.。
        let short_fp: String = fingerprint.chars().take(12).collect();
        let host_name = format!("{short_fp}.local.");
        let props = [
            (TXT_FINGERPRINT, fingerprint),
            (TXT_NAME, name),
            (TXT_PLATFORM, platform.unwrap_or("")),
            (TXT_VERSION, app_version.unwrap_or("")),
        ];
        // ip 传空 + enable_addr_auto()：由 mdns-sd 自动探测并跟踪本机接口地址。
        let info = ServiceInfo::new(SERVICE_TYPE, &short_fp, &host_name, "", port, &props[..])
            .map_err(map_mdns_err)?
            .enable_addr_auto();
        daemon.register(info).map_err(map_mdns_err)?;
        Ok(Self { daemon })
    }
}

impl Drop for MdnsAdvertiser {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// mDNS 浏览器：发现局域网内的 fluxdown 设备，解析后经 `sink` 汇出
/// [`DiscoveredPeer`]。持有 daemon 句柄，`Drop` 时优雅关闭。
pub struct MdnsBrowser {
    daemon: ServiceDaemon,
}

impl MdnsBrowser {
    /// 开始浏览，把解析出的设备推送到 `sink`（满/关闭即静默丢弃，不阻塞）。
    pub fn start(sink: mpsc::Sender<DiscoveredPeer>) -> LinkResult<Self> {
        let daemon = ServiceDaemon::new().map_err(map_mdns_err)?;
        let receiver = daemon.browse(SERVICE_TYPE).map_err(map_mdns_err)?;
        tokio::spawn(async move {
            while let Ok(event) = receiver.recv_async().await {
                if let ServiceEvent::ServiceResolved(info) = event
                    && let Some(peer) = resolved_to_peer(&info)
                {
                    // 接收端满或已关闭：丢弃本条，继续浏览。
                    let _ = sink.try_send(peer);
                }
            }
        });
        Ok(Self { daemon })
    }
}

impl Drop for MdnsBrowser {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// 把解析出的 mDNS 服务映射为 [`DiscoveredPeer`]（取首个 IPv4 地址）。
fn resolved_to_peer(info: &mdns_sd::ResolvedService) -> Option<DiscoveredPeer> {
    let addr = info.get_addresses_v4().into_iter().next()?;
    let fingerprint = info
        .get_property_val_str(TXT_FINGERPRINT)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let name = info
        .get_property_val_str(TXT_NAME)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| addr.to_string());
    let platform = info
        .get_property_val_str(TXT_PLATFORM)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let app_version = info
        .get_property_val_str(TXT_VERSION)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(DiscoveredPeer {
        fingerprint,
        name,
        platform,
        host: addr.to_string(),
        port: info.get_port(),
        app_version,
        kind: DiscoveryKind::Mdns,
    })
}

/// 手动地址探测：GET `http://host:port/ping`，解析设备身份/名称/平台/版本。
/// 供「本地配对」的「手动输入地址」兜底路径（Docker bridge / AP 隔离等 mDNS 失效场景）。
pub async fn probe(client: &reqwest::Client, host: &str, port: u16) -> LinkResult<DiscoveredPeer> {
    let url = format!("http://{host}:{port}/ping");
    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(|e| LinkError::Io(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(LinkError::Unreachable);
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| LinkError::Io(e.to_string()))?;
    let get = |k: &str| {
        json.get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Ok(DiscoveredPeer {
        fingerprint: get("linkFingerprint"),
        name: get("linkName").unwrap_or_else(|| host.to_string()),
        platform: get("linkPlatform"),
        host: host.to_string(),
        port,
        app_version: get("version"),
        kind: DiscoveryKind::Manual,
    })
}

/// 计算本机朝向 `peer_host` 的出站本地 IP（UDP connect 技巧，不真正发包），
/// 拼成 Direct 候选 `ip:port`（`api_port` = 本机 fluxdown API 端口）。
///
/// 供配对时向对端自报可达地址（对端存为回连候选）。探测失败返回空列表。
#[must_use]
pub fn local_direct_addrs(peer_host: &str, api_port: u16) -> Vec<String> {
    let Ok(peer_ip) = peer_host.parse::<IpAddr>() else {
        return Vec::new();
    };
    let bind = if peer_ip.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let Ok(sock) = UdpSocket::bind(bind) else {
        return Vec::new();
    };
    // connect 只设置默认目的地，不发包；随后 local_addr 给出朝该目的地的本地 IP。
    if sock.connect((peer_ip, api_port.max(1))).is_err() {
        return Vec::new();
    }
    match sock.local_addr() {
        Ok(local) => vec![format!("{}:{}", local.ip(), api_port)],
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn local_addr_towards_loopback_is_loopback() {
        let addrs = local_direct_addrs("127.0.0.1", 17800);
        assert_eq!(addrs.len(), 1);
        assert!(addrs[0].starts_with("127.0.0.1:") || addrs[0].starts_with("127."));
        assert!(addrs[0].ends_with(":17800"));
    }

    #[test]
    fn local_addr_towards_garbage_is_empty() {
        assert!(local_direct_addrs("not-an-ip", 17800).is_empty());
    }
}
