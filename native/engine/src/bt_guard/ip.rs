//! IP 归一、分组与局域网豁免判定。

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use super::ranges::IpRange;

/// 归一：IPv4-mapped IPv6（`::ffff:a.b.c.d`）与 NAT64 `64:ff9b::/96` 内嵌 v4 → IPv4。
pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            let seg = v6.segments();
            if seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2..6].iter().all(|s| *s == 0) {
                let o = v6.octets();
                return IpAddr::V4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
            }
            ip
        }
    }
}

/// 局域网 / 回环 / 链路本地 / CGNAT / ULA / 未指定地址（输入应已归一）。
pub fn is_lan(ip: IpAddr) -> bool {
    match normalize_ip(ip) {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xffc0) == 0xfe80
                || (seg0 & 0xfe00) == 0xfc00
        }
    }
}

/// 对端分组：IPv4 为 /32，IPv6 按前缀聚合（已掩码）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IpGroup {
    V4(Ipv4Addr),
    V6 { net: Ipv6Addr, prefix: u8 },
}

fn v6_mask(prefix: u8) -> u128 {
    match prefix {
        0 => 0,
        p if p >= 128 => u128::MAX,
        p => u128::MAX << (128 - u32::from(p)),
    }
}

impl IpGroup {
    /// `ipv6_prefix` 钳制到 48..=128。
    pub fn of(ip: IpAddr, ipv6_prefix: u8) -> Self {
        match normalize_ip(ip) {
            IpAddr::V4(v4) => Self::V4(v4),
            IpAddr::V6(v6) => {
                let prefix = ipv6_prefix.clamp(48, 128);
                let net = Ipv6Addr::from(u128::from(v6) & v6_mask(prefix));
                Self::V6 { net, prefix }
            }
        }
    }

    /// 该分组覆盖的地址区间。
    pub fn range(&self) -> IpRange {
        match *self {
            Self::V4(v4) => {
                let n = u32::from(v4);
                IpRange::V4 { start: n, end: n }
            }
            Self::V6 { net, prefix } => {
                let start = u128::from(net);
                IpRange::V6 {
                    start,
                    end: start | !v6_mask(prefix),
                }
            }
        }
    }

    /// 条目用的规范 CIDR 文本（`1.2.3.4/32` / `2001:db8::/64`）。
    pub fn cidr_text(&self) -> String {
        match self {
            Self::V4(v4) => format!("{v4}/32"),
            Self::V6 { net, prefix } => format!("{net}/{prefix}"),
        }
    }
}

/// 分组文本：`1.2.3.4` / `2001:db8::/64`。
impl fmt::Display for IpGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V4(v4) => write!(f, "{v4}"),
            Self::V6 { net, prefix } => write!(f, "{net}/{prefix}"),
        }
    }
}

impl FromStr for IpGroup {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        match s.split_once('/') {
            None => match s.parse::<IpAddr>().map_err(|e| format!("{s:?}: {e}"))? {
                IpAddr::V4(v4) => Ok(Self::V4(v4)),
                IpAddr::V6(v6) => Ok(Self::V6 {
                    net: v6,
                    prefix: 128,
                }),
            },
            Some((addr, prefix)) => {
                let addr: IpAddr = addr.parse().map_err(|e| format!("{s:?}: {e}"))?;
                let prefix: u8 = prefix.parse().map_err(|e| format!("{s:?}: {e}"))?;
                match addr {
                    IpAddr::V4(v4) if prefix == 32 => Ok(Self::V4(v4)),
                    IpAddr::V6(v6) if (1..=128).contains(&prefix) => Ok(Self::V6 {
                        net: Ipv6Addr::from(u128::from(v6) & v6_mask(prefix)),
                        prefix,
                    }),
                    _ => Err(format!("{s:?}: unsupported group prefix")),
                }
            }
        }
    }
}
