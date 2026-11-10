//! 地址区间、CIDR 解析 / 分解与排序区间索引（二分查询）。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use super::ip::normalize_ip;

/// 闭区间 `[start, end]`；两个地址族分开存放。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IpRange {
    V4 { start: u32, end: u32 },
    V6 { start: u128, end: u128 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeError(pub String);

impl std::fmt::Display for RangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RangeError {}

fn err<T>(msg: impl Into<String>) -> Result<T, RangeError> {
    Err(RangeError(msg.into()))
}

fn v4_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    }
}

fn v6_mask(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - u32::from(prefix))
    }
}

/// 宽网段上限：IPv4 跨度 ≤ 2^16（/16），IPv6 跨度 ≤ 2^96（/32）。
const WIDE_V4_SPAN: u32 = 1 << 16;
const WIDE_V6_SPAN: u128 = 1 << 96;

impl IpRange {
    /// CIDR（主机位被掩掉）。v4-mapped / NAT64 前缀 ≥ 96 的地址折算为 IPv4 区间。
    pub fn from_cidr(addr: IpAddr, prefix: u8) -> Result<Self, RangeError> {
        match addr {
            IpAddr::V4(v4) => {
                if prefix > 32 {
                    return err(format!("IPv4 prefix out of range: /{prefix}"));
                }
                let start = u32::from(v4) & v4_mask(prefix);
                Ok(Self::V4 {
                    start,
                    end: start | !v4_mask(prefix),
                })
            }
            IpAddr::V6(v6) => {
                if prefix > 128 {
                    return err(format!("IPv6 prefix out of range: /{prefix}"));
                }
                if prefix >= 96
                    && let IpAddr::V4(v4) = normalize_ip(IpAddr::V6(v6))
                {
                    return Self::from_cidr(IpAddr::V4(v4), prefix - 96);
                }
                let start = u128::from(v6) & v6_mask(prefix);
                Ok(Self::V6 {
                    start,
                    end: start | !v6_mask(prefix),
                })
            }
        }
    }

    /// `1.2.3.4` / `1.2.3.0/24` / `2001:db8::/32`。
    pub fn parse_cidr(text: &str) -> Result<Self, RangeError> {
        let text = text.trim();
        match text.split_once('/') {
            None => {
                let ip: IpAddr = text
                    .parse()
                    .map_err(|e| RangeError(format!("{text:?}: {e}")))?;
                let ip = normalize_ip(ip);
                let prefix = if ip.is_ipv4() { 32 } else { 128 };
                Self::from_cidr(ip, prefix)
            }
            Some((addr, prefix)) => {
                let ip: IpAddr = addr
                    .trim()
                    .parse()
                    .map_err(|e| RangeError(format!("{text:?}: {e}")))?;
                let prefix: u8 = prefix
                    .trim()
                    .parse()
                    .map_err(|e| RangeError(format!("{text:?}: {e}")))?;
                Self::from_cidr(ip, prefix)
            }
        }
    }

    /// 任意起止地址（同族，`start <= end`；先归一）。
    pub fn from_addrs(start: IpAddr, end: IpAddr) -> Result<Self, RangeError> {
        match (normalize_ip(start), normalize_ip(end)) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                let (a, b) = (u32::from(a), u32::from(b));
                if a > b {
                    return err("range start is after end");
                }
                Ok(Self::V4 { start: a, end: b })
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => {
                let (a, b) = (u128::from(a), u128::from(b));
                if a > b {
                    return err("range start is after end");
                }
                Ok(Self::V6 { start: a, end: b })
            }
            _ => err("range endpoints are of different address families"),
        }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self, normalize_ip(ip)) {
            (Self::V4 { start, end }, IpAddr::V4(v4)) => (*start..=*end).contains(&u32::from(v4)),
            (Self::V6 { start, end }, IpAddr::V6(v6)) => (*start..=*end).contains(&u128::from(v6)),
            _ => false,
        }
    }

    /// 宽于 IPv4 /16 或 IPv6 /32。
    pub fn is_wide(&self) -> bool {
        match *self {
            Self::V4 { start, end } => end - start >= WIDE_V4_SPAN,
            Self::V6 { start, end } => end - start >= WIDE_V6_SPAN,
        }
    }

    /// 区间是否恰为一个 CIDR 块，是则返回 `(网络地址, 前缀)`。
    pub fn as_cidr(&self) -> Option<(IpAddr, u8)> {
        match *self {
            Self::V4 { start, end } => {
                let span = u64::from(end - start) + 1;
                (span.is_power_of_two() && start.trailing_zeros() >= span.trailing_zeros()).then(
                    || {
                        (
                            IpAddr::V4(Ipv4Addr::from(start)),
                            32 - span.trailing_zeros() as u8,
                        )
                    },
                )
            }
            Self::V6 { start, end } => {
                let diff = end - start;
                let (block_bits, ok) = if diff == u128::MAX {
                    (128, start == 0)
                } else {
                    let span = diff + 1;
                    (
                        span.trailing_zeros(),
                        span.is_power_of_two() && start.trailing_zeros() >= span.trailing_zeros(),
                    )
                };
                ok.then(|| (IpAddr::V6(Ipv6Addr::from(start)), 128 - block_bits as u8))
            }
        }
    }

    /// 规范 CIDR 文本；非单块区间返回 `None`。
    pub fn cidr_string(&self) -> Option<String> {
        self.as_cidr().map(|(ip, prefix)| format!("{ip}/{prefix}"))
    }

    /// 最小 CIDR 分解（块数 ≤ 2×位宽）。
    pub fn to_cidr_ranges(&self) -> Vec<IpRange> {
        let (mut cur, end, bits, is_v4) = match *self {
            Self::V4 { start, end } => (u128::from(start), u128::from(end), 32u32, true),
            Self::V6 { start, end } => (start, end, 128u32, false),
        };
        let mut out = Vec::new();
        loop {
            let tz = if cur == 0 {
                bits
            } else {
                cur.trailing_zeros().min(bits)
            };
            let remaining = end - cur;
            let span_log = if remaining == u128::MAX {
                128
            } else {
                (remaining + 1).ilog2()
            };
            let k = tz.min(span_log);
            let block_end = if k >= 128 {
                u128::MAX
            } else {
                cur + ((1u128 << k) - 1)
            };
            out.push(if is_v4 {
                Self::V4 {
                    start: cur as u32,
                    end: block_end as u32,
                }
            } else {
                Self::V6 {
                    start: cur,
                    end: block_end,
                }
            });
            if block_end >= end {
                break;
            }
            cur = block_end + 1;
        }
        out
    }
}

/// 按起点排序的区间数组 + 前缀最大终点，二分定位后向前回扫，支持任意重叠。
/// 载荷为 `u32`（调用方的条目下标）。
#[derive(Debug, Default, Clone)]
pub struct RangeIndex {
    v4: Vec<(u32, u32, u32)>,
    v4_max_end: Vec<u32>,
    v6: Vec<(u128, u128, u32)>,
    v6_max_end: Vec<u128>,
}

impl RangeIndex {
    pub fn build(items: impl IntoIterator<Item = (IpRange, u32)>) -> Self {
        let mut v4 = Vec::new();
        let mut v6 = Vec::new();
        for (range, payload) in items {
            match range {
                IpRange::V4 { start, end } => v4.push((start, end, payload)),
                IpRange::V6 { start, end } => v6.push((start, end, payload)),
            }
        }
        v4.sort_unstable();
        v6.sort_unstable();
        let mut v4_max_end = Vec::with_capacity(v4.len());
        let mut max = 0u32;
        for (_, end, _) in &v4 {
            max = max.max(*end);
            v4_max_end.push(max);
        }
        let mut v6_max_end = Vec::with_capacity(v6.len());
        let mut max = 0u128;
        for (_, end, _) in &v6 {
            max = max.max(*end);
            v6_max_end.push(max);
        }
        Self {
            v4,
            v4_max_end,
            v6,
            v6_max_end,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.v4.is_empty() && self.v6.is_empty()
    }

    /// 对每个覆盖 `ip` 的区间调用 `f(payload)`。`ip` 须已归一。
    pub fn for_each_match(&self, ip: IpAddr, mut f: impl FnMut(u32)) {
        match ip {
            IpAddr::V4(v4) => {
                let needle = u32::from(v4);
                let upper = self.v4.partition_point(|r| r.0 <= needle);
                for (&(_, end, payload), &max_end) in
                    self.v4.iter().zip(&self.v4_max_end).take(upper).rev()
                {
                    if max_end < needle {
                        break;
                    }
                    if end >= needle {
                        f(payload);
                    }
                }
            }
            IpAddr::V6(v6) => {
                let needle = u128::from(v6);
                let upper = self.v6.partition_point(|r| r.0 <= needle);
                for (&(_, end, payload), &max_end) in
                    self.v6.iter().zip(&self.v6_max_end).take(upper).rev()
                {
                    if max_end < needle {
                        break;
                    }
                    if end >= needle {
                        f(payload);
                    }
                }
            }
        }
    }
}
