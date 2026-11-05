//! 按本机有效物理内存分档的内存预算。
//!
//! 有效内存 = min(物理内存, cgroup 限额)：Linux / Android 读 `/proc/meminfo` 与
//! cgroup v2 `memory.max`（沿层级向上取最小）/ v1 `memory.limit_in_bytes`，覆盖
//! Docker、systemd `MemoryMax=` 与 NAS 套件限额。其它平台探测不到时按标准档。
//! 进程内首次使用时探测一次并记一条日志。

use std::sync::LazyLock;

use crate::logger::log_info;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// 内存档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryTier {
    /// 有效内存 < 1.5 GiB（1G 级 NAS / 软路由 / 容器限额）。
    Low,
    /// 1.5 GiB ≤ 有效内存 < 3 GiB。
    Medium,
    /// 其余，或探测不到。
    Standard,
}

/// 各子系统按档位取用的缓冲上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryBudget {
    pub tier: MemoryTier,
    /// 单个 HLS 任务「已下载、待按序写盘」分段的字节上限。
    pub hls_reorder_bytes: u64,
    /// SQLite 每连接页缓存上限（KiB）。按需增长，空库不预占。
    pub sqlite_cache_kib: u32,
}

/// 由有效内存推导预算。
pub(crate) fn budget_for(effective_bytes: Option<u64>) -> MemoryBudget {
    let tier = match effective_bytes {
        Some(bytes) if bytes < GIB * 3 / 2 => MemoryTier::Low,
        Some(bytes) if bytes < GIB * 3 => MemoryTier::Medium,
        _ => MemoryTier::Standard,
    };
    let (hls_reorder_bytes, sqlite_cache_kib) = match tier {
        MemoryTier::Low => (8 * MIB, 4 * 1024),
        MemoryTier::Medium => (16 * MIB, 8 * 1024),
        MemoryTier::Standard => (32 * MIB, 16 * 1024),
    };
    MemoryBudget {
        tier,
        hls_reorder_bytes,
        sqlite_cache_kib,
    }
}

/// 本进程的内存预算（首次使用时探测）。
static BUDGET: LazyLock<MemoryBudget> = LazyLock::new(|| {
    let effective = effective_memory();
    let budget = budget_for(effective);
    log_info!(
        "[mem-budget] effective memory {} MiB -> {:?} (hls reorder {} MiB, sqlite cache {} MiB/conn)",
        effective.map_or_else(|| "unknown".to_owned(), |b| (b / MIB).to_string()),
        budget.tier,
        budget.hls_reorder_bytes / MIB,
        budget.sqlite_cache_kib / 1024
    );
    budget
});

/// 本进程的内存预算。
pub(crate) fn current() -> &'static MemoryBudget {
    &BUDGET
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn effective_memory() -> Option<u64> {
    let total = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| parse_meminfo_total(&text));
    let limit = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|text| cgroup_limit(&text, |path| std::fs::read_to_string(path).ok()));
    min_known(total, limit)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn effective_memory() -> Option<u64> {
    None
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
fn min_known(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
/// `/proc/meminfo` 的 `MemTotal:  <n> kB`。
fn parse_meminfo_total(text: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
/// 限额文件内容：`max` / 空 / 不可解析视为无限额；v1 的「无限」是接近 `i64::MAX`
/// 的页对齐值，按超过 1 EiB 视为无限额。
fn parse_limit(text: &str) -> Option<u64> {
    let value: u64 = text.trim().parse().ok()?;
    (value > 0 && value < (1 << 60)).then_some(value)
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
/// 从 `/proc/self/cgroup` 定位本进程所在 cgroup，取 v2 层级上（含祖先）最小的
/// `memory.max`，没有 v2 时取 v1 memory 控制器的 `memory.limit_in_bytes`。
fn cgroup_limit(proc_self_cgroup: &str, read: impl Fn(&str) -> Option<String>) -> Option<u64> {
    let mut v2_path = None;
    let mut v1_path = None;
    for line in proc_self_cgroup.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(id), Some(controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if id == "0" && controllers.is_empty() {
            v2_path = Some(path);
        } else if controllers.split(',').any(|c| c == "memory") {
            v1_path = Some(path);
        }
    }

    if let Some(path) = v2_path {
        let mut limit = None;
        let mut current = path.trim_end_matches('/');
        loop {
            let file = format!("/sys/fs/cgroup{current}/memory.max");
            limit = min_known(limit, read(&file).as_deref().and_then(parse_limit));
            match current.rsplit_once('/') {
                Some((parent, _)) => current = parent,
                None => break,
            }
        }
        if limit.is_some() {
            return limit;
        }
    }
    let path = v1_path?.trim_end_matches('/');
    let own = read(&format!(
        "/sys/fs/cgroup/memory{path}/memory.limit_in_bytes"
    ));
    let root = read("/sys/fs/cgroup/memory/memory.limit_in_bytes");
    min_known(
        own.as_deref().and_then(parse_limit),
        root.as_deref().and_then(parse_limit),
    )
}

#[cfg(test)]
mod tests {
    use super::{GIB, MemoryTier, budget_for, cgroup_limit, parse_meminfo_total};
    use std::collections::HashMap;

    #[test]
    fn tiers_follow_effective_memory() {
        // 1G 板子的 MemTotal 通常只有 900 多 MiB；4G 机器约 3.7 GiB。
        assert_eq!(budget_for(Some(GIB - (80 << 20))).tier, MemoryTier::Low);
        assert_eq!(budget_for(Some(GIB * 3 / 2)).tier, MemoryTier::Medium);
        assert_eq!(
            budget_for(Some(GIB * 2 - (100 << 20))).tier,
            MemoryTier::Medium
        );
        assert_eq!(budget_for(Some(GIB * 3)).tier, MemoryTier::Standard);
        assert_eq!(budget_for(None).tier, MemoryTier::Standard);
        assert!(budget_for(Some(GIB)).hls_reorder_bytes < budget_for(None).hls_reorder_bytes);
    }

    #[test]
    fn meminfo_total_is_parsed_in_bytes() {
        let text = "MemTotal:        1918212 kB\nMemFree:          123456 kB\n";
        assert_eq!(parse_meminfo_total(text), Some(1_918_212 * 1024));
    }

    #[test]
    fn cgroup_v2_takes_smallest_limit_along_the_hierarchy() {
        let files: HashMap<&str, &str> = HashMap::from([
            (
                "/sys/fs/cgroup/system.slice/fluxdown.service/memory.max",
                "max\n",
            ),
            ("/sys/fs/cgroup/system.slice/memory.max", "805306368\n"),
            ("/sys/fs/cgroup/memory.max", "max\n"),
        ]);
        let read = |path: &str| files.get(path).map(|s| (*s).to_owned());
        assert_eq!(
            cgroup_limit("0::/system.slice/fluxdown.service\n", read),
            Some(805_306_368)
        );
    }

    #[test]
    fn cgroup_unlimited_values_are_ignored() {
        let v2 = |path: &str| (path == "/sys/fs/cgroup/memory.max").then(|| "max\n".to_owned());
        assert_eq!(cgroup_limit("0::/\n", v2), None);

        let v1 = |path: &str| {
            path.ends_with("memory.limit_in_bytes")
                .then(|| "9223372036854771712\n".to_owned())
        };
        assert_eq!(cgroup_limit("4:memory:/docker/abc\n", v1), None);
        let v1_limited = |path: &str| {
            (path == "/sys/fs/cgroup/memory/docker/abc/memory.limit_in_bytes")
                .then(|| "536870912\n".to_owned())
        };
        assert_eq!(
            cgroup_limit(
                "12:cpu,cpuacct:/docker/abc\n4:memory:/docker/abc\n",
                v1_limited
            ),
            Some(536_870_912)
        );
    }
}
