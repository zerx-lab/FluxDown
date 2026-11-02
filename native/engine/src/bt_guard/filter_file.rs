//! 过滤文件订阅解析。
//!
//! 每行一种格式（空行、`#` / `;` / `//` 注释行忽略）：
//! - 单 IP / CIDR：`1.2.3.4`、`1.2.3.0/24`、`2001:db8::/32`（行尾 ` # 注释` 被剥掉）；
//! - 起止范围：`1.2.3.4-1.2.3.9`（连字符两侧允许空白）；
//! - PeerGuardian：`名称:起点-终点`（名称可含冒号，仅 IPv4）；
//! - eMule `ipfilter.dat`：`起点 - 终点 , 访问级别 , 描述`。eMule 语义：访问级别 ≥ 127 的行
//!   不属于过滤范围，跳过；级别缺失 / 非数字视为无效行。
//!
//! 宽于 IPv4 /16、IPv6 /32 的条目整条丢弃并计数（宽网段守卫）。

use std::net::IpAddr;

use super::ranges::IpRange;

/// 单个来源最多接受的区间数，超出部分被丢弃并标记 `truncated`。
pub const MAX_FILTER_RANGES: usize = 500_000;
/// 单个来源拆成 CIDR 块后最多保留的条目数，超出部分丢弃并在来源状态里记录。
pub const MAX_FILTER_ENTRIES_PER_SOURCE: usize = 200_000;
/// eMule 访问级别阈值：不低于该值的行不过滤。
const EMULE_PASS_LEVEL: u32 = 127;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedFilter {
    pub ranges: Vec<IpRange>,
    pub dropped_wide: usize,
    pub invalid_lines: usize,
    /// eMule 行因访问级别 ≥ 127 被跳过的数量。
    pub skipped_by_level: usize,
    pub truncated: bool,
}

enum LineParse {
    Range(IpRange),
    SkippedByLevel,
    Invalid,
    Ignore,
}

/// 严格解析优先；eMule 文件常见的前导零（`001.002.003.000`）按十进制接受。
fn parse_ip(text: &str) -> Option<IpAddr> {
    let text = text.trim();
    if let Ok(ip) = text.parse() {
        return Some(ip);
    }
    let mut octets = [0u8; 4];
    let mut parts = text.split('.');
    for slot in &mut octets {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    parts.next().is_none().then(|| IpAddr::from(octets))
}

/// `a-b`（连字符两侧可有空白）。IPv6 地址不含 `-`，直接按第一个 `-` 切分。
fn parse_range_pair(text: &str) -> Option<IpRange> {
    let (a, b) = text.split_once('-')?;
    IpRange::from_addrs(parse_ip(a)?, parse_ip(b)?).ok()
}

fn parse_line(raw: &str) -> LineParse {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with("//") {
        return LineParse::Ignore;
    }
    if line.contains(',') {
        // eMule：`start - end , level , desc`
        let mut parts = line.splitn(3, ',');
        let (Some(range), Some(level)) = (parts.next(), parts.next()) else {
            return LineParse::Invalid;
        };
        let Some(range) = parse_range_pair(range) else {
            return LineParse::Invalid;
        };
        let Ok(level) = level.trim().parse::<u32>() else {
            return LineParse::Invalid;
        };
        return if level >= EMULE_PASS_LEVEL {
            LineParse::SkippedByLevel
        } else {
            LineParse::Range(range)
        };
    }
    // 行尾注释。
    let line = line.split_once('#').map_or(line, |(head, _)| head).trim();
    if line.is_empty() {
        return LineParse::Ignore;
    }
    if let Ok(range) = IpRange::parse_cidr(line) {
        return LineParse::Range(range);
    }
    if let Some(range) = parse_range_pair(line) {
        return LineParse::Range(range);
    }
    // PeerGuardian：名称可含 ':'，取最后一个 ':' 之后的 v4 范围。
    if let Some((_, tail)) = line.rsplit_once(':')
        && let Some(range @ IpRange::V4 { .. }) = parse_range_pair(tail)
    {
        return LineParse::Range(range);
    }
    LineParse::Invalid
}

pub fn parse_filter_file(text: &str) -> ParsedFilter {
    let mut out = ParsedFilter::default();
    for (index, raw) in text.lines().enumerate() {
        let raw = if index == 0 {
            raw.trim_start_matches('\u{feff}')
        } else {
            raw
        };
        match parse_line(raw) {
            LineParse::Ignore => {}
            LineParse::Invalid => out.invalid_lines += 1,
            LineParse::SkippedByLevel => out.skipped_by_level += 1,
            LineParse::Range(range) => {
                if range.is_wide() {
                    out.dropped_wide += 1;
                } else if out.ranges.len() >= MAX_FILTER_RANGES {
                    out.truncated = true;
                } else {
                    out.ranges.push(range);
                }
            }
        }
    }
    out
}
