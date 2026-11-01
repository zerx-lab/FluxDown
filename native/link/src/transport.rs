//! 数据面直连：拨号已配对设备的候选地址并以 `/ping` 复核身份。
//!
//! 只支持网络可达直连（同局域网 / 已配端口转发 / 公网反代），不做 NAT 穿透。

use std::time::Duration;

use super::address::PeerAddress;
use super::error::LinkError;
use super::types::PeerRecord;

/// 拨通一台已配对设备后得到的连接句柄。
#[derive(Debug, Clone)]
pub struct PeerConn {
    /// 对端 fluxdown API 基址（`http(s)://host[:port][/base]`，不含尾斜杠）。
    pub base_url: String,
    /// 对端 `/ping` 自报的数据面链路协议版本（旧版对端没有该字段 = 0）。发起方据此在发出
    /// 数据面请求之前判定 [`LinkError::PeerOutdated`]。
    pub link_protocol: u32,
}

/// 网络可达直连：拨号候选（`ip:port` 或完整 URL）并 `/ping` 探活。
pub struct DirectTransport {
    client: reqwest::Client,
    probe_timeout: Duration,
}

impl DirectTransport {
    /// 用给定 HTTP 客户端构造（客户端应配 `.no_proxy()` 且不跟随重定向，见
    /// [`crate::wire::http_client`]）。
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            probe_timeout: Duration::from_secs(3),
        }
    }

    /// 建立到 `peer` 的已鉴权数据面端点。没有候选或全部不可达时返回
    /// [`LinkError::Unreachable`]。
    pub async fn connect(&self, peer: &PeerRecord) -> Result<PeerConn, LinkError> {
        // 依次尝试全部 Direct 候选而非只试第一个：候选列表可能同时保留 mDNS 新发现的地址
        // （未探测、纯启发式排序，见 `crate::discovery::pick_best_v4` 文档）、对端自报的地址与
        // 配对时验证过的旧地址——只试首个会让一次误选直接判死本该可达的设备。任一候选
        // 连通即返回；全部候选都失败才失败。
        //
        // 每个候选失败的**根因**（DNS / 连接被拒 / TLS / 非 2xx / 非 JSON）都记进日志，
        // 不再被 `continue` 吞掉——「设备显示离线」时才有线索可查。
        //
        // `/ping` 指纹不符（旧地址被 DHCP 回收给了同网段另一台 FluxDown 设备）只说明**这个
        // 候选**不是目标：记录后继续试后面的候选，否则旧 IP 上的别的设备会永远挡住新地址。
        // 全部候选耗尽、没有任何候选通过，且出现过不符时才返回 `IdentityMismatch`（留下
        // 「地址上是别人」的线索）；这不会把冒充降级成成功——通过的候选必须指纹一致，数据面
        // 的响应还要经链路密钥解封（见 `LinkManager::post_sealed`）。
        let mut mismatch: Option<String> = None;
        for candidate in &peer.candidates {
            let addr = candidate.address.as_str();
            let address = match PeerAddress::parse(addr) {
                Ok(address) => address,
                Err(error) => {
                    tracing::debug!(
                        fingerprint = %peer.fingerprint,
                        candidate = addr,
                        error = %error,
                        "skipping unparsable link candidate"
                    );
                    continue;
                }
            };
            // 直连以 /ping 探活判定可达；失败即视为该候选不可达，试下一个。
            let resp = match self
                .client
                .get(address.url("/ping"))
                .timeout(self.probe_timeout)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(error) => {
                    tracing::debug!(
                        fingerprint = %peer.fingerprint,
                        candidate = addr,
                        error = %super::wire::error_chain(&error),
                        "link candidate probe failed"
                    );
                    continue;
                }
            };
            if !resp.status().is_success() {
                tracing::debug!(
                    fingerprint = %peer.fingerprint,
                    candidate = addr,
                    status = %resp.status(),
                    "link candidate /ping returned a non-success status"
                );
                continue;
            }
            // TOFU 身份复核：`/ping` 200 响应不代表就是配对时那台——必须比对指纹。JSON 解析
            // 失败视同该候选不可达。
            let body = match resp.json::<serde_json::Value>().await {
                Ok(body) => body,
                Err(error) => {
                    tracing::debug!(
                        fingerprint = %peer.fingerprint,
                        candidate = addr,
                        error = %super::wire::error_chain(&error),
                        "link candidate /ping body is not JSON"
                    );
                    continue;
                }
            };
            match verify_ping_identity(&body, &peer.fingerprint, addr) {
                Ok(link_protocol) => {
                    return Ok(PeerConn {
                        base_url: address.base_url(),
                        link_protocol,
                    });
                }
                Err(LinkError::IdentityMismatch(_)) => {
                    tracing::debug!(
                        fingerprint = %peer.fingerprint,
                        candidate = addr,
                        "link candidate answered with another device identity; trying the next candidate"
                    );
                    mismatch.get_or_insert_with(|| addr.to_owned());
                }
                Err(other) => return Err(other),
            }
        }
        match mismatch {
            Some(addr) => Err(LinkError::IdentityMismatch(addr)),
            None => Err(LinkError::Unreachable),
        }
    }
}

/// 从 `/ping` 响应体校验对端身份指纹——[`DirectTransport::connect`] 的 TOFU
/// 复核核心逻辑，抽成纯函数便于单测（无需起真实 HTTP 服务器即可覆盖比对
/// 分支）。指纹一致才算通过，并返回对端自报的 `linkProtocol`（旧版对端没有 = 0）；
/// 缺字段或不一致一律视为身份不符。
fn verify_ping_identity(
    body: &serde_json::Value,
    expect_fp: &str,
    addr: &str,
) -> Result<u32, LinkError> {
    match body.get("linkFingerprint").and_then(|v| v.as_str()) {
        Some(fp) if fp == expect_fp => Ok(body
            .get("linkProtocol")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .unwrap_or(0)),
        _ => Err(LinkError::IdentityMismatch(addr.to_string())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::types::{PeerCandidate, PeerRecord, TransportKind};

    fn peer_with(candidates: Vec<PeerCandidate>) -> PeerRecord {
        PeerRecord {
            fingerprint: "fp".to_string(),
            identity_pub: vec![0u8; 32],
            name: "n".to_string(),
            link_secret: vec![0u8; 32],
            candidates,
            ..PeerRecord::default()
        }
    }

    #[tokio::test]
    async fn connect_without_candidates_is_unreachable() {
        let t = DirectTransport::new(reqwest::Client::new());
        let err = t.connect(&peer_with(vec![])).await.unwrap_err();
        assert!(matches!(err, LinkError::Unreachable));
    }

    #[tokio::test]
    async fn direct_connect_unreachable_on_dead_address() {
        let t = DirectTransport::new(reqwest::Client::new());
        // 一个不监听的端口（TEST-NET-1，不可路由）→ Unreachable，不 panic。
        let peer = peer_with(vec![PeerCandidate {
            kind: TransportKind::Direct,
            address: "192.0.2.1:9".to_string(),
        }]);
        let err = t.connect(&peer).await.unwrap_err();
        assert!(matches!(err, LinkError::Unreachable));
    }

    #[test]
    fn verify_ping_identity_accepts_matching_fingerprint() {
        let body = serde_json::json!({ "linkFingerprint": "abc123", "linkProtocol": 2 });
        assert_eq!(
            verify_ping_identity(&body, "abc123", "10.0.0.1:1").unwrap(),
            2
        );
        // 旧版对端没有 linkProtocol：按 0 处理（发起方据此判 PeerOutdated）。
        let legacy = serde_json::json!({ "linkFingerprint": "abc123" });
        assert_eq!(
            verify_ping_identity(&legacy, "abc123", "10.0.0.1:1").unwrap(),
            0
        );
    }

    #[test]
    fn verify_ping_identity_rejects_mismatched_fingerprint() {
        // 地址被 DHCP 回收复用/篡改指向了另一台设备：指纹对不上，必须拒绝
        // 而不是静默连接——否则任务会被下发给冒充者。
        let body = serde_json::json!({ "linkFingerprint": "someone-else" });
        let err = verify_ping_identity(&body, "abc123", "10.0.0.1:1").unwrap_err();
        assert!(matches!(err, LinkError::IdentityMismatch(addr) if addr == "10.0.0.1:1"));
    }

    #[test]
    fn verify_ping_identity_rejects_missing_fingerprint_field() {
        // 对端没有开启 link（`/ping` 不带 linkFingerprint）：同样不能当作
        // 身份匹配处理。
        let body = serde_json::json!({ "success": true });
        let err = verify_ping_identity(&body, "abc123", "10.0.0.1:1").unwrap_err();
        assert!(matches!(err, LinkError::IdentityMismatch(_)));
    }

    /// 起一个只会回应 `/ping` 的假对端，声称自己的指纹是 `fingerprint`。
    async fn fake_ping_server(fingerprint: &str) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = serde_json::json!({
            "success": true,
            "app": "FluxDown",
            "linkFingerprint": fingerprint,
            "linkProtocol": 2,
        })
        .to_string();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    if let Err(error) = socket.read(&mut buf).await {
                        tracing::debug!(%error, "fake ping client closed early");
                        return;
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    if let Err(error) = socket.write_all(response.as_bytes()).await {
                        tracing::debug!(%error, "fake ping client closed before the reply");
                        return;
                    }
                    if let Err(error) = socket.shutdown().await {
                        tracing::debug!(%error, "fake ping shutdown failed");
                    }
                });
            }
        });
        addr
    }

    fn direct(addr: &std::net::SocketAddr) -> PeerCandidate {
        PeerCandidate {
            kind: TransportKind::Direct,
            address: addr.to_string(),
        }
    }

    #[tokio::test]
    async fn connect_skips_a_candidate_that_now_belongs_to_another_device() {
        // 旧 IP 被同网段另一台 FluxDown 设备占用（指纹不符）：不能挡住后面的新地址。
        let squatter = fake_ping_server("someone-else").await;
        let target = fake_ping_server("fp").await;
        let transport = DirectTransport::new(reqwest::Client::new());
        let conn = transport
            .connect(&peer_with(vec![direct(&squatter), direct(&target)]))
            .await
            .unwrap();
        assert_eq!(conn.base_url, format!("http://{target}"));
        assert_eq!(conn.link_protocol, 2);
    }

    #[tokio::test]
    async fn connect_reports_identity_mismatch_only_when_every_candidate_failed() {
        let squatter = fake_ping_server("someone-else").await;
        let transport = DirectTransport::new(reqwest::Client::new());
        // 只有冒充者：全部失败且出现过不符 → IdentityMismatch。
        let err = transport
            .connect(&peer_with(vec![direct(&squatter)]))
            .await
            .unwrap_err();
        assert!(matches!(err, LinkError::IdentityMismatch(_)), "{err:?}");
        // 不符之后的候选不可达（端口无人监听）：仍是 IdentityMismatch，而不是被吞成离线。
        let closed = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap()
        };
        let err = transport
            .connect(&peer_with(vec![direct(&squatter), direct(&closed)]))
            .await
            .unwrap_err();
        assert!(matches!(err, LinkError::IdentityMismatch(_)), "{err:?}");
    }
}
