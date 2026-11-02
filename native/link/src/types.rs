//! 设备互联领域类型。
//!
//! 这些类型是子系统内部的领域模型（非 wire 契约）。HTTP wire 类型定义在
//! `fluxdown_protocol::daemon`；宿主层负责在两者之间转换。
//! 本模块只描述如何表达发现设备、已配对设备和传输候选。

use serde::{Deserialize, Serialize};

use crate::address::PeerAddress;

/// 设备被发现的途径。
///
/// **扩展点**：未来新增发现方式（如登录账户名册回填、二维码带地址）只需加变体，
/// 发现层 [`crate::discovery`] 的实现互不影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoveryKind {
    /// mDNS 局域网自动发现（`_fluxdown._tcp.local.`）。
    Mdns,
    /// 用户手动输入地址后 `/ping` 探测。
    Manual,
}

/// 到达一台**已知**设备数据面的连接方式：只有网络可达直连，无 NAT 穿透。
///
/// 作为 [`PeerCandidate::kind`] 写入已配对设备名册与配对报文（序列化为 `"Direct"`），
/// 保留枚举形态以维持该线格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportKind {
    /// 网络可达直连：直接拨号候选 `ip:port`（或完整 URL）。
    Direct,
}

/// 一台在网络上被发现、**尚未配对**的设备。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPeer {
    /// 对端 Ed25519 身份指纹（经 `/ping` TOFU 获得；未探测到时为 `None`）。
    pub fingerprint: Option<String>,
    /// 设备展示名。
    pub name: String,
    /// 平台标识（windows/macos/linux/android/ios/server）。
    pub platform: Option<String>,
    /// 可达地址（IPv4/IPv6/域名，不含方括号）。
    pub host: String,
    /// 对端 fluxdown API 端口。
    pub port: u16,
    /// 对端客户端版本（`/ping` 透出）。
    pub app_version: Option<String>,
    /// 发现途径。
    pub kind: DiscoveryKind,
}

/// 到达已配对设备的一个候选端点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCandidate {
    /// 该候选走哪种传输。
    pub kind: TransportKind,
    /// 地址：`ip:port` 或完整 `http(s)://host[:port][/base]`，见 [`crate::address::PeerAddress`]。
    pub address: String,
}

/// 已配对设备名册里 Direct 候选端点集合的总数上限。已验证的新地址去重后置首、旧候选整体保留
/// 作回退；若不设上限，设备长期在多个网络间漂移会让候选表无界增长。取一个小值：日常场景
/// 「当前网段 + 一两个历史网段」远用不到 4 个，仍能覆盖「NAS 同时插着物理网卡和虚拟机 host-only
/// 网卡」这类多网卡场景的合理候选数。对端经已认证链路自报的回连地址同样受它约束。
pub const MAX_DIRECT_CANDIDATES: usize = 4;

/// 经**已认证链路**（`/api/v1/link/info`）双向交换的设备信息。
///
/// 同一结构既是本机发给对端的自报信息，也是从对端收到的信息；收到的值一律经过清洗
/// （见 [`crate::text`]），地址已按 [`crate::PeerAddress`] 校验并规范化。`path_style` 用小写
/// 字符串（`"windows"` / `"posix"`）而不是协议枚举：本 crate 不依赖宿主协议层，由宿主自行映射。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    /// 设备自报名（不含对端侧备注名）。
    pub name: Option<String>,
    /// 平台标识。
    pub platform: Option<String>,
    /// 客户端版本。
    pub app_version: Option<String>,
    /// 自报的可回连地址（至多 [`MAX_DIRECT_CANDIDATES`] 条，规范化的候选形态）。
    pub addrs: Vec<String>,
    /// 已登录云账号时的云设备 ID；未登录为 `None`。**只经已认证链路告知已配对对端**。
    pub cloud_device_id: Option<String>,
    /// 默认下载目录（设备本地路径）。
    pub default_save_dir: Option<String>,
    /// 路径风格：`"windows"` | `"posix"`。
    pub path_style: Option<String>,
}

/// 一台**已配对**并持久化的设备。是本地设备名册的行模型。
///
/// 除 `custom_name` / `revoked` 是本机侧状态外，其余「对端自报」字段（名称、平台、版本、云设备
/// ID、目录、路径风格、回连候选）都会随 [`PeerRecord::apply_peer_info`] 更新。新增字段一律
/// `serde(default)`：旧版本落盘的记录照常读回。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerRecord {
    /// 设备 ID = 对端 Ed25519 公钥指纹（hex(sha256(pub))，64 hex）。主键。
    pub fingerprint: String,
    /// 对端 Ed25519 身份公钥（32 字节）：配对握手时用于验证双方签名（见
    /// `pairing` 模块）。TOFU 身份复核**实际发生在**
    /// [`crate::transport::DirectTransport::connect`]：每次拨号都会
    /// 比对 `/ping` 响应体的 `linkFingerprint` 与本记录的 `fingerprint`
    /// （本字段的派生值），而非直接重放本字段——地址被 DHCP 回收复用或被
    /// 篡改指向攻击者时，指纹不符即拒绝连接。
    pub identity_pub: Vec<u8>,
    /// 对端自报的设备展示名（不含本机备注名；展示用 [`PeerRecord::effective_name`]）。
    pub name: String,
    /// 平台标识。
    pub platform: Option<String>,
    /// 配对时经 X25519 ECDH 派生的**每对设备独立**共享密钥（32 字节），
    /// 用于数据面链路鉴权（HMAC）。绝不上网络明文传输。
    pub link_secret: Vec<u8>,
    /// 已知候选端点（按优先级尝试）。
    pub candidates: Vec<PeerCandidate>,
    /// 配对时间（Unix 秒）。
    pub paired_at: i64,
    /// 最近一次成功连接/探活时间（Unix 秒）。
    pub last_seen_at: i64,
    /// 对端自报的客户端版本。
    #[serde(default)]
    pub app_version: Option<String>,
    /// 对端自报的云设备 ID（对端已登录云账号时）。
    #[serde(default)]
    pub cloud_device_id: Option<String>,
    /// 对端自报的默认下载目录。
    #[serde(default)]
    pub default_save_dir: Option<String>,
    /// 对端自报的路径风格：`"windows"` | `"posix"`。
    #[serde(default)]
    pub path_style: Option<String>,
    /// 本机给该设备设置的备注名（仅本机可见，优先于对端自报名）。
    #[serde(default)]
    pub custom_name: Option<String>,
    /// 对端已解除与本机的配对（经已认证链路通知）：拒绝其入站请求、本机不再向它下发；
    /// 重新配对同一设备会覆盖记录并清除该标记。
    #[serde(default)]
    pub revoked: bool,
    /// 响应方侧：该对端被本机授权可经 `/rpc` 完全控制**本机**（hello 带 `request_control` 且本机
    /// 用户批准）。重新配对取本次结果，不沿用旧记录。
    #[serde(default)]
    pub grants_control: bool,
    /// 发起方侧：本机可控制该对端（本机以 `request_control` 发起且配对完成）。重新配对取本次结果，
    /// 不沿用旧记录。
    #[serde(default)]
    pub controllable: bool,
}

impl PeerRecord {
    /// 展示用短指纹（前 12 hex）。
    #[must_use]
    pub fn short_fingerprint(&self) -> String {
        self.fingerprint.chars().take(12).collect()
    }

    /// 首个候选地址（供数据面拼 base_url）。
    #[must_use]
    pub fn direct_address(&self) -> Option<&str> {
        self.candidates.first().map(|c| c.address.as_str())
    }

    /// 生效展示名：本机备注名优先，否则对端自报名。
    #[must_use]
    pub fn effective_name(&self) -> &str {
        self.custom_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.name)
    }

    /// 把对端经已认证链路自报的信息并入本记录；返回是否有任何字段**真的变化**（调用方只在
    /// 变化时落盘 / 通知，空闲时无变化就零写入）。
    ///
    /// - 名称：非空才覆盖；平台 / 版本：给了才覆盖。
    /// - 云设备 ID / 默认目录 / 路径风格：以对端最新自报为准（未给 = 清除，例如对端登出云账号）。
    /// - 回连地址：校验并规范化后置于候选首位，去重并截断到 [`MAX_DIRECT_CANDIDATES`]；
    ///   对端没有自报地址时候选原样保留。
    pub fn apply_peer_info(&mut self, info: &PeerInfo) -> bool {
        let mut changed = false;
        if let Some(name) = info.name.as_ref().filter(|name| !name.is_empty())
            && self.name != *name
        {
            self.name.clone_from(name);
            changed = true;
        }
        if info.platform.is_some() && self.platform != info.platform {
            self.platform.clone_from(&info.platform);
            changed = true;
        }
        if info.app_version.is_some() && self.app_version != info.app_version {
            self.app_version.clone_from(&info.app_version);
            changed = true;
        }
        if self.cloud_device_id != info.cloud_device_id {
            self.cloud_device_id.clone_from(&info.cloud_device_id);
            changed = true;
        }
        if self.default_save_dir != info.default_save_dir {
            self.default_save_dir.clone_from(&info.default_save_dir);
            changed = true;
        }
        if self.path_style != info.path_style {
            self.path_style.clone_from(&info.path_style);
            changed = true;
        }
        let merged = merge_reported_candidates(&self.candidates, &info.addrs);
        if merged != self.candidates {
            self.candidates = merged;
            changed = true;
        }
        changed
    }
}

/// 把对端自报的回连地址并入候选：每条经 [`PeerAddress::parse`] 校验并规范化（非法的丢弃），
/// 按自报顺序置首，旧候选去重后接在后面，整体截断到 [`MAX_DIRECT_CANDIDATES`]。
/// 没有任何合法自报地址时原样返回旧候选。
#[must_use]
pub fn merge_reported_candidates(old: &[PeerCandidate], reported: &[String]) -> Vec<PeerCandidate> {
    let mut fresh: Vec<PeerCandidate> = Vec::new();
    for address in reported {
        let Ok(parsed) = PeerAddress::parse(address) else {
            continue;
        };
        let candidate = PeerCandidate {
            kind: TransportKind::Direct,
            address: parsed.to_candidate(),
        };
        if !fresh.contains(&candidate) {
            fresh.push(candidate);
        }
    }
    if fresh.is_empty() {
        return old.to_vec();
    }
    let mut merged = fresh;
    for candidate in old {
        if !merged.contains(candidate) {
            merged.push(candidate.clone());
        }
    }
    merged.truncate(MAX_DIRECT_CANDIDATES);
    merged
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn direct(address: &str) -> PeerCandidate {
        PeerCandidate {
            kind: TransportKind::Direct,
            address: address.to_owned(),
        }
    }

    fn record() -> PeerRecord {
        PeerRecord {
            fingerprint: "fp".into(),
            name: "Laptop".into(),
            candidates: vec![direct("10.0.0.2:17800")],
            ..PeerRecord::default()
        }
    }

    #[test]
    fn reported_addresses_go_first_deduplicated_and_truncated() {
        let merged = merge_reported_candidates(
            &[direct("10.0.0.2:17800"), direct("10.0.0.3:17800")],
            &[
                "10.0.0.9:17800".into(),
                "http://10.0.0.2:17800".into(),
                "ftp://nope".into(),
                "10.0.0.8:17800".into(),
                "10.0.0.7:17800".into(),
            ],
        );
        assert_eq!(
            merged,
            vec![
                direct("10.0.0.9:17800"),
                direct("10.0.0.2:17800"),
                direct("10.0.0.8:17800"),
                direct("10.0.0.7:17800"),
            ]
        );
        // 没有合法自报地址：旧候选原样保留。
        assert_eq!(
            merge_reported_candidates(&[direct("10.0.0.2:17800")], &["ftp://nope".into()]),
            vec![direct("10.0.0.2:17800")]
        );
    }

    #[test]
    fn applying_the_same_info_twice_changes_nothing() {
        let mut record = record();
        let info = PeerInfo {
            name: Some("Laptop 2".into()),
            platform: Some("macos".into()),
            app_version: Some("1.2.3".into()),
            addrs: vec!["10.0.0.9:17800".into()],
            cloud_device_id: Some("cloud-1".into()),
            default_save_dir: Some("/Users/me/Downloads".into()),
            path_style: Some("posix".into()),
        };
        assert!(record.apply_peer_info(&info));
        assert_eq!(record.name, "Laptop 2");
        assert_eq!(record.cloud_device_id.as_deref(), Some("cloud-1"));
        assert_eq!(record.candidates[0], direct("10.0.0.9:17800"));
        assert_eq!(record.candidates[1], direct("10.0.0.2:17800"));
        // 空闲时再次交换同样的信息：没有任何变化 → 调用方不写盘、不通知。
        assert!(!record.apply_peer_info(&info));
    }

    #[test]
    fn peer_logout_clears_cloud_device_id_but_keeps_a_missing_name() {
        let mut record = record();
        record.cloud_device_id = Some("cloud-1".into());
        let info = PeerInfo {
            name: None,
            ..PeerInfo::default()
        };
        assert!(record.apply_peer_info(&info));
        assert_eq!(record.cloud_device_id, None);
        assert_eq!(record.name, "Laptop");
    }

    #[test]
    fn custom_name_wins_over_the_reported_name() {
        let mut record = record();
        assert_eq!(record.effective_name(), "Laptop");
        record.custom_name = Some("书房".into());
        assert_eq!(record.effective_name(), "书房");
        record.apply_peer_info(&PeerInfo {
            name: Some("Laptop 2".into()),
            ..PeerInfo::default()
        });
        assert_eq!(record.effective_name(), "书房");
        assert_eq!(record.name, "Laptop 2");
        record.custom_name = Some(String::new());
        assert_eq!(record.effective_name(), "Laptop 2");
    }
}
