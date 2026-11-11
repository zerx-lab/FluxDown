//! BitTorrent 配置与链接识别——与 `bt` feature 无关、始终编译的纯数据部分。
//!
//! 宿主（daemon / CLI）与管理器要在 P2P 被编译期剔除（`--no-default-features`）
//! 的构建里照常构造 [`BtConfig`]、识别 magnet 链接并给出明确拒绝，所以这里
//! **不得**引用 librqbit 或任何 BT 运行时代码；会话 / 下载实现在 `bt_downloader`
//! （仅 `bt` feature 下编译）。

/// FluxDown-owned MSE policy. Keeping this type at the engine boundary avoids
/// exposing the concrete BitTorrent backend to host crates.
///
/// ```
/// use fluxdown_engine::bt_config::BtMseMode;
///
/// assert_eq!(BtMseMode::from("forced"), BtMseMode::Forced);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BtMseMode {
    /// Reject encrypted peer handshakes and use plaintext only.
    Disabled,
    /// Prefer encrypted handshakes while retaining plaintext compatibility.
    #[default]
    Enabled,
    /// Require encryption for every peer connection.
    Forced,
}

impl From<&str> for BtMseMode {
    fn from(value: &str) -> Self {
        let value = value.trim();
        if value.eq_ignore_ascii_case("disabled") {
            Self::Disabled
        } else if value.eq_ignore_ascii_case("forced") {
            Self::Forced
        } else {
            Self::Enabled
        }
    }
}

/// Returns `true` if the URL looks like a magnet link.
pub fn is_magnet_url(url: &str) -> bool {
    url.get(..8)
        .map(|prefix| prefix.eq_ignore_ascii_case("magnet:?"))
        .unwrap_or(false)
}

/// User-configurable BT session settings, loaded from the DB config table.
#[derive(Debug, Clone)]
pub struct BtConfig {
    pub enabled: bool,
    pub enable_dht: bool,
    pub enable_upnp: bool,
    /// Listen and connect over uTP in addition to TCP (`ListenerMode::TcpAndUtp`).
    /// Many peers (uTorrent / qBittorrent / BitComet) are reachable over uTP
    /// only from behind NAT; #825 measured ~2× live peers with it on.
    pub enable_utp: bool,
    pub port_start: u16,
    pub port_end: u16,
    /// User-supplied extra tracker URLs (newline-separated).
    /// These are **merged** with the built-in `PUBLIC_TRACKERS` list.
    pub custom_trackers: String,
    /// Trackers fetched from subscription sources (newline-separated cache,
    /// see `tracker_subscription`).  Empty when the subscription feature is
    /// disabled.  Merged + deduped with `custom_trackers` at session build.
    pub subscription_trackers: String,
    /// Stop seeding when the total share ratio reaches this value (0 = unlimited).
    pub seed_ratio_limit: f64,
    /// Stop seeding when the post-completion share ratio reaches this value
    /// (`(uploaded - uploaded_at_completion) / downloaded`, 0 = unlimited).
    pub seed_post_ratio_limit: f64,
    /// Stop seeding after this many minutes (0 = unlimited).
    pub seed_time_limit_minutes: u64,
    /// Stop seeding after this many minutes of zero upload speed (0 = unlimited).
    pub seed_inactive_time_limit_minutes: u64,
    /// How to combine the ratio, seed-time and inactive-time limits.
    pub seed_limit_operator: crate::bt_seeding::SeedingLimitOperator,
    /// What to do once a seeding limit is reached.
    pub seed_then_action: String,
    /// Max simultaneously active seeders (0 = unlimited). Completed torrents
    /// beyond the cap wait in a FIFO seeding queue until a slot frees up.
    pub seed_max_active: usize,
    /// MSE (Message Stream Encryption) policy for BT peer connections.
    /// Emergency escape hatch settable via config API/CLI; no Settings UI.
    pub mse_mode: BtMseMode,
    /// 每个种子同时上传给多少个对端（`bt_upload_slots`）：`0` = 自动（按上传速率伸缩，
    /// 至少 4 个），其余为固定槽数。会话级选项，改动经重建共享会话生效。
    pub upload_slots: u32,
    /// 新建 BT 任务的默认选片方式（`bt_piece_order`）：`true` = 按顺序下载，默认 `false` =
    /// 最稀有优先。只作用于此后新建的任务。
    pub sequential: bool,
    /// 新建 BT 任务默认开启「先下载首尾文件块」（`bt_first_last_pieces_first`，默认关）。
    /// 只作用于此后新建的任务。
    pub first_last_pieces_first: bool,
    /// 本地节点发现（LSD，`bt_enable_lsd`）。会话级，改动经重建共享会话生效。
    pub enable_lsd: bool,
    /// uTP 拥塞控制用 LEDBAT（`bt_utp_congestion` = `ledbat`，默认）：检测到链路排队就让路；
    /// `false` = CUBIC，只按丢包退让。会话级。
    pub utp_ledbat: bool,
    /// 全部任务合计的连接数上限（`bt_max_connections`，连接中 + 已连接；`0` = 不限）。热生效。
    pub max_connections: u32,
    /// 每个任务的连接数上限（`bt_max_connections_per_torrent`，连接中 + 已连接）。热生效。
    pub max_connections_per_torrent: u32,
    /// HTTP 源（BEP 19 webseed，`bt_enable_webseed`）。会话级。
    pub enable_webseed: bool,
    /// tracker 汇报策略（BEP 12）：向所有层级汇报（`bt_announce_all_tiers`）。会话级。
    pub announce_all_tiers: bool,
    /// tracker 汇报策略：向层级内所有 tracker 汇报（`bt_announce_all_trackers`）。会话级。
    pub announce_all_trackers: bool,
    /// BT 经「代理」页的手动代理服务器出站（`bt_proxy_enabled`）。会话级；实际代理地址由
    /// 管理器在建会话时从全局代理配置取。
    pub proxy_enabled: bool,
    /// 对端连接（与 DHT）也走代理（`bt_proxy_peers`）；`false` = 只有 tracker 与 HTTP 源走代理。
    pub proxy_peers: bool,
    /// I2P（`bt_i2p_enabled`）：经本机 I2P 路由的 SAM 桥下载 I2P 种子。会话级。
    pub i2p_enabled: bool,
    /// SAM 桥主机（`bt_i2p_sam_host`）。
    pub i2p_sam_host: String,
    /// SAM 桥端口（`bt_i2p_sam_port`）。
    pub i2p_sam_port: u16,
    /// I2P 种子同时允许明网对端（`bt_i2p_mixed`）。
    pub i2p_mixed: bool,
}

/// 每个任务的连接数上限缺省值（`bt_max_connections_per_torrent` 的默认，见
/// `bt_downloader.rs` 对 512 的说明：librqbit 的上限同时计「连接中」与「已连接」）。
pub const DEFAULT_BT_PEER_LIMIT: u32 = 512;

impl BtConfig {
    /// 本构建是否编译了 BitTorrent 后端（`bt` feature）。
    pub const COMPILED: bool = cfg!(feature = "bt");

    /// 生效配置：没有 BT 后端的构建里 `enabled` 恒为 `false`，让管理器沿用
    /// 「BT 被禁用」的全部拒绝路径（建任务 / 恢复 / 入队），无需另起一套。
    #[must_use]
    pub fn effective(mut self) -> Self {
        self.enabled &= Self::COMPILED;
        self
    }
}

impl Default for BtConfig {
    fn default() -> Self {
        Self {
            enabled: Self::COMPILED,
            enable_dht: true,
            enable_upnp: true,
            enable_utp: true,
            port_start: 6881,
            port_end: 6891,
            custom_trackers: String::new(),
            subscription_trackers: String::new(),
            seed_ratio_limit: 0.0,
            seed_post_ratio_limit: 0.0,
            seed_time_limit_minutes: 0,
            seed_inactive_time_limit_minutes: 0,
            seed_limit_operator: crate::bt_seeding::SeedingLimitOperator::Or,
            seed_then_action: "stop".to_string(),
            seed_max_active: 0,
            mse_mode: BtMseMode::Enabled,
            upload_slots: 0,
            sequential: false,
            first_last_pieces_first: false,
            enable_lsd: true,
            utp_ledbat: true,
            max_connections: 0,
            max_connections_per_torrent: DEFAULT_BT_PEER_LIMIT,
            enable_webseed: true,
            announce_all_tiers: true,
            announce_all_trackers: true,
            proxy_enabled: false,
            proxy_peers: true,
            i2p_enabled: false,
            i2p_sam_host: "127.0.0.1".to_string(),
            i2p_sam_port: 7656,
            i2p_mixed: false,
        }
    }
}
