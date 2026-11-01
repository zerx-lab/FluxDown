//! 证据上报：弹出 daemon 的待上报证据，批量 POST 到 FluxCloud `/api/v1/bt-guard/evidence`。
//!
//! 这是遥测红线例外（用户 2026-10-31 决定）里 BT 防吸血证据的**唯一出口**：只有登录用户、
//! 用户显式开启 `bt_guard_report_enabled` 才会走到这里；证据内容由 daemon 按契约组装（不含文件名 /
//! 路径 / 种子名，私有种子不报），本模块不增删字段。
//!
//! 失败的批次留在内存队列里等待重试（上限 [`RETAIN_CAP`]，超出丢最旧），退避期间没有任何定时器
//! 之外的活动；登出 / 切换账号 / 关闭上报开关即清空。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use fluxdown_protocol::BtGuardEvidenceItemDto;
use serde::{Deserialize, Serialize};

use super::daemon::GuardDaemon;
use crate::cloud::{CloudApi, CloudError};

/// 单个请求的证据条数上限（FluxCloud 契约）。
pub(crate) const BATCH_SIZE: usize = 200;
/// 内存中保留（等待重试）的证据条数上限。
pub(crate) const RETAIN_CAP: usize = 1000;
/// `clientVersion` 的长度上限（服务端校验）。
const CLIENT_VERSION_MAX_CHARS: usize = 32;

/// 上报失败退避：1 min 起翻倍，封顶 1 h（每日配额耗尽时 429 也按此节奏重试）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct UploadTiming {
    pub(crate) first: Duration,
    pub(crate) cap: Duration,
}

impl UploadTiming {
    pub(crate) const PRODUCTION: Self = Self {
        first: Duration::from_secs(60),
        cap: Duration::from_secs(60 * 60),
    };

    pub(crate) fn delay(self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(20);
        self.first.saturating_mul(1_u32 << doublings).min(self.cap)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceRequest<'a> {
    client_version: &'a str,
    items: &'a [BtGuardEvidenceItemDto],
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct EvidenceReply {
    accepted: u32,
    rejected: u32,
}

/// 一次失败该怎么处置。
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// 网络 / 限流 / 服务端暂时不可用：保留批次，退避后重试。
    Retry,
    /// 请求本身被拒（4xx）：重试没有意义，丢弃这一批。
    Drop,
    /// 登录会话已结束：队列整体作废。
    SessionEnded,
}

fn classify(error: &CloudError) -> Failure {
    if error.code.as_deref() == Some("session_changed") {
        return Failure::SessionEnded;
    }
    if error.unreachable || error.retryable || matches!(error.status, Some(429 | 500..=599)) {
        Failure::Retry
    } else {
        Failure::Drop
    }
}

pub(crate) struct EvidenceUploader {
    queue: VecDeque<BtGuardEvidenceItemDto>,
    timing: UploadTiming,
    failures: u32,
    retry_at: Option<Instant>,
}

impl EvidenceUploader {
    pub(crate) fn new(timing: UploadTiming) -> Self {
        Self {
            queue: VecDeque::new(),
            timing,
            failures: 0,
            retry_at: None,
        }
    }

    /// 下一次重试的时刻；队列为空或没有失败时为 `None`。
    pub(crate) fn retry_at(&self) -> Option<Instant> {
        self.retry_at
    }

    /// 内存中等待上报的条数。
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.queue.len()
    }

    /// 丢弃全部保留的证据并取消重试（登出 / 换号 / 关闭上报）。
    pub(crate) fn clear(&mut self) {
        if !self.queue.is_empty() {
            tracing::debug!(count = self.queue.len(), "BT guard evidence queue cleared");
        }
        self.queue.clear();
        self.failures = 0;
        self.retry_at = None;
    }

    /// 入队新弹出的证据；总量超过 [`RETAIN_CAP`] 时丢最旧的。
    fn enqueue(&mut self, items: Vec<BtGuardEvidenceItemDto>) {
        self.queue.extend(items);
        let excess = self.queue.len().saturating_sub(RETAIN_CAP);
        if excess > 0 {
            self.queue.drain(..excess);
            tracing::warn!(
                dropped = excess,
                "BT guard evidence queue full; dropped the oldest"
            );
        }
    }

    /// 弹出 daemon 的待上报证据并发送到队列清空，或遇到需要退避的失败为止。
    ///
    /// 调用方必须先确认：已登录且 `bt_guard_report_enabled`。登录会话以进入时的纪元为准，
    /// 期间登出 / 换号的响应不会被当成当前账号的结果。
    pub(crate) async fn flush(&mut self, cloud: &CloudApi, daemon: &dyn GuardDaemon) {
        let api = cloud.at_epoch(cloud.request_epoch());
        match daemon.drain_evidence().await {
            Ok(items) => self.enqueue(items),
            Err(error) => tracing::debug!(error = %error, "BT guard evidence drain failed"),
        }
        let version: String = fluxdown_protocol::APP_VERSION
            .chars()
            .take(CLIENT_VERSION_MAX_CHARS)
            .collect();
        while !self.queue.is_empty() {
            let take = self.queue.len().min(BATCH_SIZE);
            let Some(batch) = self.queue.make_contiguous().get(..take) else {
                break;
            };
            let request = EvidenceRequest {
                client_version: &version,
                items: batch,
            };
            match api.bt_guard_evidence::<_, EvidenceReply>(&request).await {
                Ok(reply) => {
                    tracing::debug!(
                        sent = take,
                        accepted = reply.accepted,
                        rejected = reply.rejected,
                        "BT guard evidence uploaded"
                    );
                    self.queue.drain(..take);
                    self.failures = 0;
                    self.retry_at = None;
                }
                Err(error) => match classify(&error) {
                    Failure::Retry => {
                        self.failures = self.failures.saturating_add(1);
                        let delay = self.timing.delay(self.failures);
                        self.retry_at = Some(Instant::now() + delay);
                        tracing::debug!(
                            error = %error,
                            pending = self.queue.len(),
                            retry_secs = delay.as_secs(),
                            "BT guard evidence upload failed; will retry"
                        );
                        return;
                    }
                    Failure::SessionEnded => {
                        self.clear();
                        return;
                    }
                    Failure::Drop => {
                        tracing::warn!(error = %error, dropped = take, "FluxCloud rejected a BT guard evidence batch");
                        self.queue.drain(..take);
                    }
                },
            }
        }
        // 队列已清空（Retry / SessionEnded 都提前返回）：不留过期的重试时刻，否则工作循环会空转。
        self.failures = 0;
        self.retry_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(n: u64) -> BtGuardEvidenceItemDto {
        BtGuardEvidenceItemDto {
            delivered_bytes: n,
            ..BtGuardEvidenceItemDto::default()
        }
    }

    fn error(status: Option<u16>, retryable: bool, unreachable: bool, code: &str) -> CloudError {
        CloudError {
            status,
            code: Some(code.to_owned()),
            message: String::new(),
            retryable,
            unreachable,
        }
    }

    #[test]
    fn production_backoff_is_one_minute_doubling_to_one_hour() {
        let secs = |failures| UploadTiming::PRODUCTION.delay(failures).as_secs();
        assert_eq!(secs(1), 60);
        assert_eq!(secs(2), 120);
        assert_eq!(secs(6), 1920);
        assert_eq!(secs(7), 3600);
        assert_eq!(secs(u32::MAX), 3600);
    }

    #[test]
    fn failures_are_classified_by_whether_retrying_can_help() {
        assert_eq!(
            classify(&error(None, false, true, "network")),
            Failure::Retry
        );
        assert_eq!(
            classify(&error(Some(429), false, false, "rate")),
            Failure::Retry
        );
        assert_eq!(
            classify(&error(Some(503), false, false, "x")),
            Failure::Retry
        );
        assert_eq!(
            classify(&error(Some(500), true, false, "x")),
            Failure::Retry
        );
        assert_eq!(
            classify(&error(Some(400), false, false, "validation")),
            Failure::Drop
        );
        assert_eq!(
            classify(&error(Some(401), false, false, "unauthorized")),
            Failure::Drop
        );
        assert_eq!(
            classify(&error(None, false, false, "session_changed")),
            Failure::SessionEnded
        );
    }

    #[test]
    fn queue_keeps_the_newest_thousand_and_drops_the_oldest() {
        let mut uploader = EvidenceUploader::new(UploadTiming::PRODUCTION);
        uploader.enqueue((0..800).map(item).collect());
        uploader.enqueue((800..1300).map(item).collect());
        assert_eq!(uploader.pending(), RETAIN_CAP);
        assert_eq!(uploader.queue.front().map(|i| i.delivered_bytes), Some(300));
        assert_eq!(uploader.queue.back().map(|i| i.delivered_bytes), Some(1299));
        // 单次超过上限（daemon 一次弹出多于 1000）同样只留最新的 1000 条。
        uploader.enqueue((2000..3500).map(item).collect());
        assert_eq!(uploader.pending(), RETAIN_CAP);
        assert_eq!(
            uploader.queue.front().map(|i| i.delivered_bytes),
            Some(2500)
        );
    }

    #[test]
    fn clear_forgets_items_failures_and_the_retry_deadline() {
        let mut uploader = EvidenceUploader::new(UploadTiming::PRODUCTION);
        uploader.enqueue(vec![item(1)]);
        uploader.failures = 3;
        uploader.retry_at = Some(Instant::now());
        uploader.clear();
        assert_eq!(uploader.pending(), 0);
        assert_eq!(uploader.failures, 0);
        assert!(uploader.retry_at().is_none());
    }
}
