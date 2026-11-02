//! `BtGuardConfig`：字段与契约 §3 设置键（去掉 `bt_guard_` 前缀）一一对应，`Default` 即默认值。

use std::str::FromStr;

/// 检测规则 / 云名单的运行模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum RuleMode {
    Off,
    #[default]
    Observe,
    Enforce,
}

impl RuleMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }
}

impl FromStr for RuleMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "observe" => Ok(Self::Observe),
            "enforce" => Ok(Self::Enforce),
            other => Err(format!("invalid rule mode: {other:?}")),
        }
    }
}

/// 下载中任务（本端尚未拥有全部分片）的动作上限。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DownloadingMaxAction {
    /// 只记录，不执行任何动作。
    Observe,
    #[default]
    Throttle,
    Choke,
    Ban,
}

impl DownloadingMaxAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Throttle => "throttle",
            Self::Choke => "choke",
            Self::Ban => "ban",
        }
    }
}

impl FromStr for DownloadingMaxAction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "observe" => Ok(Self::Observe),
            "throttle" => Ok(Self::Throttle),
            "choke" => Ok(Self::Choke),
            "ban" => Ok(Self::Ban),
            other => Err(format!("invalid downloading max action: {other:?}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtGuardConfig {
    pub enabled: bool,
    pub rule_excessive: RuleMode,
    pub rule_rerequest: RuleMode,
    pub rule_rewind: RuleMode,
    pub rule_lag: RuleMode,
    pub rule_fingerprint: RuleMode,
    pub cloud_mode: RuleMode,
    /// 110..=1000
    pub excess_ratio_pct: u32,
    /// 0..=1048576
    pub min_torrent_mib: u64,
    /// 0..=100
    pub rewind_tolerance_pct: u32,
    /// 受限队列速率（KiB/s，1..=1048576）。由宿主下发给 BT 会话；规则逻辑不读取。
    pub throttle_kib: u32,
    /// 1..=720
    pub ban_base_hours: u32,
    /// 1..=365
    pub ban_max_days: u32,
    pub downloading_max_action: DownloadingMaxAction,
    pub skip_private: bool,
    pub skip_lan: bool,
    /// 48..=128
    pub ipv6_prefix: u8,
    /// 每行一个 URL。
    pub filter_urls: String,
    pub cloud_enabled: bool,
    /// 证据上报开关（设备本地）：仅决定是否在内存累积待上报证据。
    pub report_enabled: bool,
}

impl Default for BtGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rule_excessive: RuleMode::Observe,
            rule_rerequest: RuleMode::Observe,
            rule_rewind: RuleMode::Observe,
            rule_lag: RuleMode::Observe,
            rule_fingerprint: RuleMode::Observe,
            cloud_mode: RuleMode::Observe,
            excess_ratio_pct: 200,
            min_torrent_mib: 64,
            rewind_tolerance_pct: 7,
            throttle_kib: 32,
            ban_base_hours: 24,
            ban_max_days: 30,
            downloading_max_action: DownloadingMaxAction::Throttle,
            skip_private: true,
            skip_lan: true,
            ipv6_prefix: 64,
            filter_urls: String::new(),
            cloud_enabled: true,
            report_enabled: false,
        }
    }
}

impl BtGuardConfig {
    /// 钳制到契约范围（daemon 校验之外的兜底，保证内部算术不溢出、不除零）。
    pub fn sanitized(mut self) -> Self {
        self.excess_ratio_pct = self.excess_ratio_pct.clamp(110, 1000);
        self.min_torrent_mib = self.min_torrent_mib.min(1_048_576);
        self.rewind_tolerance_pct = self.rewind_tolerance_pct.min(100);
        self.throttle_kib = self.throttle_kib.clamp(1, 1_048_576);
        self.ban_base_hours = self.ban_base_hours.clamp(1, 720);
        self.ban_max_days = self.ban_max_days.clamp(1, 365);
        self.ipv6_prefix = self.ipv6_prefix.clamp(48, 128);
        self
    }

    pub(crate) fn min_bytes(&self) -> u64 {
        self.min_torrent_mib.saturating_mul(1024 * 1024)
    }

    /// `filter_urls` 逐行去空白、去空行、去重（保序）。
    pub fn filter_url_list(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for line in self.filter_urls.lines() {
            let line = line.trim();
            if !line.is_empty() && !out.iter().any(|u| u == line) {
                out.push(line.to_owned());
            }
        }
        out
    }
}
