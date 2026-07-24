//! 众包遥测采样缓冲（P2 客户端侧）。
//!
//! 引擎在两个观测点采样：connect 预筛（连接耗时/死活）与 worker 段完成
//! 回报（吞吐/失败）。样本进入进程级有界环形缓冲，去抖持久化到 config 表
//! `cdn_pending_reports`（JSON 数组）；**上传由 Dart 云服务负责**（引擎不
//! 持有云端会话）：Dart 周期读取该 key → `POST /api/v1/cdn/report` →
//! 成功后写空值清空（宿主 apply-config 分支转调 [`clear`]）。
//!
//! 隐私边界（方案 §5.3）：只采 `(host, ip, connect_ms, throughput_bps, ok)`
//! ——无 URL path/query/token/本机信息；`device_hash` 由服务端从鉴权设备
//! 派生（客户端不发送）。采样常开，不提供用户开关。
//!
//! 可靠性取舍：遥测是尽力而为——「Dart 读取与清空之间新增的样本」会丢
//! （不会重复），缓冲溢出丢最旧样本。任何失败都不影响下载功能。

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex as StdMutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::logger::log_info;

/// config 表 key：待上传样本（Dart 上报成功后写空清空）。
const PENDING_KEY: &str = "cdn_pending_reports";

/// 缓冲容量：超出丢最旧。64 条 = 服务端单次批量上限；4 批的余量足够
/// 覆盖 Dart 30min 上报周期内的正常样本量。
const MAX_PENDING: usize = 256;

/// 持久化去抖（秒）：样本高频（每段一条），逐条落盘无意义。
const PERSIST_MIN_GAP_SECS: u64 = 30;

/// 单条遥测样本（与 `POST /api/v1/cdn/report` 的 `samples[]` 元素同构）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CdnSample {
    pub host: String,
    pub ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throughput_bps: Option<u64>,
    pub ok: bool,
}

static PENDING: OnceLock<StdMutex<VecDeque<CdnSample>>> = OnceLock::new();
static LAST_PERSIST_SECS: AtomicU64 = AtomicU64::new(0);

fn pending() -> &'static StdMutex<VecDeque<CdnSample>> {
    PENDING.get_or_init(|| StdMutex::new(VecDeque::new()))
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 记录一条样本。超容量丢最旧；按去抖间隔持久化。
pub fn record(sample: CdnSample, db: &Db) {
    if let Ok(mut buf) = pending().lock() {
        if buf.len() >= MAX_PENDING {
            buf.pop_front();
        }
        buf.push_back(sample);
    }
    let now = now_unix_secs();
    let last = LAST_PERSIST_SECS.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= PERSIST_MIN_GAP_SECS
        && LAST_PERSIST_SECS
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        persist(db);
    }
}

/// connect 预筛观测点的便捷入口。
pub(crate) fn record_connect(host: &str, ip: IpAddr, connect_ms: u32, ok: bool, db: &Db) {
    record(
        CdnSample {
            host: host.to_string(),
            ip: ip.to_string(),
            connect_ms: Some(connect_ms),
            throughput_bps: None,
            ok,
        },
        db,
    );
}

/// 段完成观测点的便捷入口（仅钉定节点——SYS 无法归因具体 IP）。
pub(crate) fn record_segment(
    host: &str,
    ip: IpAddr,
    throughput_bps: Option<u64>,
    ok: bool,
    db: &Db,
) {
    record(
        CdnSample {
            host: host.to_string(),
            ip: ip.to_string(),
            connect_ms: None,
            throughput_bps,
            ok,
        },
        db,
    );
}

/// Dart 上报完成（宿主收到 `cdn_pending_reports` 空值写入）→ 清空缓冲。
/// 读取与清空之间新增的样本按设计丢弃（绝不重复上报）。
pub fn clear() {
    if let Ok(mut buf) = pending().lock() {
        buf.clear();
    }
    log_info!("[cdn-telemetry] 待上传样本已上报清空");
}

/// 启动时回读上次进程遗留的待上传样本（合并到缓冲头部）。
pub(crate) async fn load_pending(db: &Db) {
    let Ok(Some(raw)) = db.get_config(PENDING_KEY).await else {
        return;
    };
    if raw.trim().is_empty() {
        return;
    }
    let Ok(samples) = serde_json::from_str::<Vec<CdnSample>>(&raw) else {
        return;
    };
    if let Ok(mut buf) = pending().lock() {
        for s in samples.into_iter().rev() {
            if buf.len() >= MAX_PENDING {
                break;
            }
            buf.push_front(s);
        }
    }
}
/// 同步落盘当前缓冲快照（await 完成）。供宿主在 Dart `RequestConfig`
/// 读取 config 前调用，保证上报读到的 `cdn_pending_reports` 含全部
/// 内存样本——否则去抖窗口尾部的样本（下载结束后不再有新 record 触发
/// persist）会一直滞留内存、随进程退出丢失。缓冲为空时不写（避免把
/// 已存在的待上传 JSON 覆盖为空数组）。
pub async fn flush(db: &Db) {
    let snapshot: Vec<CdnSample> = match pending().lock() {
        Ok(buf) => buf.iter().cloned().collect(),
        Err(_) => return,
    };
    if snapshot.is_empty() {
        return;
    }
    let Ok(json) = serde_json::to_string(&snapshot) else {
        return;
    };
    LAST_PERSIST_SECS.store(now_unix_secs(), Ordering::Relaxed);
    if let Err(e) = db.set_config(PENDING_KEY, &json).await {
        log_info!("[cdn-telemetry] flush 落盘失败（忽略）: {}", e);
    }
}

/// 把当前缓冲快照写回 config 表（fire-and-forget）。
fn persist(db: &Db) {
    let snapshot: Vec<CdnSample> = match pending().lock() {
        Ok(buf) => buf.iter().cloned().collect(),
        Err(_) => return,
    };
    let Ok(json) = serde_json::to_string(&snapshot) else {
        return;
    };
    let db = db.clone();
    tokio::spawn(async move {
        if let Err(e) = db.set_config(PENDING_KEY, &json).await {
            log_info!("[cdn-telemetry] 待上传样本持久化失败（忽略）: {}", e);
        }
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{CdnSample, MAX_PENDING, clear, pending};

    fn sample(n: u32) -> CdnSample {
        CdnSample {
            host: "t.example".to_string(),
            ip: format!("10.9.0.{}", n % 250),
            connect_ms: Some(n),
            throughput_bps: None,
            ok: true,
        }
    }

    /// 直接操作缓冲（record 需要 Db；缓冲语义与开关语义分开测）。
    #[test]
    fn buffer_caps_and_clears() {
        clear();
        {
            let mut buf = pending().lock().unwrap();
            for n in 0..(MAX_PENDING as u32 + 10) {
                if buf.len() >= MAX_PENDING {
                    buf.pop_front();
                }
                buf.push_back(sample(n));
            }
            assert_eq!(buf.len(), MAX_PENDING);
            // 丢最旧：队首应是第 10 条。
            assert_eq!(buf.front().unwrap().connect_ms, Some(10));
        }
        clear();
        assert!(pending().lock().unwrap().is_empty());
    }

    #[test]
    fn sample_serde_roundtrip_omits_none() {
        let s = CdnSample {
            host: "h".into(),
            ip: "1.2.3.4".into(),
            connect_ms: None,
            throughput_bps: Some(1_000_000),
            ok: false,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("connect_ms"), "None 字段不应出现在 JSON");
        assert!(json.contains("throughput_bps"));
        let back: CdnSample = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
