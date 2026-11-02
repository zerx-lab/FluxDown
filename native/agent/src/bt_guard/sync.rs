//! 云名单拉取状态机：条件请求 → 验签 → 序号防回滚 → 交付 daemon → 落盘缓存。
//!
//! 空闲静默：本模块没有定时器，只在 [`CloudListSync::refresh`] 被调用时联网（调用方只在 daemon
//! 发出 `BtGuardCloudRefreshWanted` 时调用）。失败退避 30 min 起翻倍、封顶 6 h，窗口内的调用不联网。
//!
//! 缓存 `<dir>/cloud.json` 是服务端信封的原样文本（临时文件 + rename 原子落盘），**每次读回都重新
//! 验签**：被篡改 / 公钥轮换后失效的缓存直接删除。只有 daemon 接受（`applied = true`）或已经持有的
//! 文档才会写入缓存，缓存序号因此永远是可信的防回滚基线。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::daemon::GuardDaemon;
use super::list::{KeySet, ListError, MAX_ENVELOPE_BYTES, VerifiedList, text_of, verify_list};
use crate::egress::{Egress, EgressAttempt, EgressClient};
use crate::signed_envelope::EnvelopeError;

/// FluxCloud 云名单接口路径（接在 FluxCloud base URL 之后）。
pub(crate) const LIST_PATH: &str = "/api/v1/bt-guard/list";
const CACHE_FILE: &str = "cloud.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// 失败退避：30 min 起每次翻倍，封顶 6 h。
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryTiming {
    pub(crate) first: Duration,
    pub(crate) cap: Duration,
}

impl RetryTiming {
    pub(crate) const PRODUCTION: Self = Self {
        first: Duration::from_secs(30 * 60),
        cap: Duration::from_secs(6 * 60 * 60),
    };

    /// 第 `failures` 次连续失败后的等待时长（`failures` 从 1 起）。
    pub(crate) fn delay(self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(20);
        self.first.saturating_mul(1_u32 << doublings).min(self.cap)
    }
}

#[derive(Debug, thiserror::Error)]
enum SyncError {
    #[error("http error: {0}")]
    Http(String),
    #[error("http status {0}")]
    Status(u16),
    #[error(transparent)]
    List(#[from] ListError),
    #[error("daemon: {0}")]
    Daemon(String),
}

/// 一次 [`CloudListSync::refresh`] 的结果。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    /// daemon 接受了新文档，缓存已更新。
    Applied { sequence: u64 },
    /// 服务端 304，或文档序号与已持有的一致：没有需要交付的新内容。
    Unchanged,
    /// 文档合法但 daemon 拒绝（差异防护 / 过期 / 金丝雀……）：保留旧名单，原因在 daemon 状态里。
    Rejected { reason: String },
    /// 仍在失败退避窗口内，没有联网；`retry_in` 为窗口剩余时长。
    BackedOff { retry_in: Duration },
    /// 失败（网络 / 状态码 / 验签 / 回滚 / daemon 不可达）：`retry_in` 后才允许再次联网。
    Failed { retry_in: Duration, error: String },
}

enum Downloaded {
    NotModified,
    Body { text: String, etag: Option<String> },
}

pub(crate) struct CloudListSync {
    dir: PathBuf,
    keys: KeySet,
    egress: Egress,
    http: EgressClient,
    timing: RetryTiming,
    loaded: bool,
    /// 磁盘缓存里（已重验）文档的序号；也是防回滚基线之一。
    cached_sequence: Option<u64>,
    /// 与 `cached_sequence` 对应文档的 `ETag`；缓存与验证器同生同灭，只在内存。
    etag: Option<String>,
    /// daemon 已拒绝过的缓存序号：不再反复交付同一份被拒文档。
    rejected_cache: Option<u64>,
    failures: u32,
    next_attempt: Option<Instant>,
}

impl CloudListSync {
    pub(crate) fn new(dir: PathBuf, keys: KeySet, egress: Egress, timing: RetryTiming) -> Self {
        Self {
            dir,
            keys,
            egress,
            http: EgressClient::new(|| {
                reqwest::Client::builder()
                    .timeout(REQUEST_TIMEOUT)
                    .user_agent(concat!("fluxdown-agent/", env!("CARGO_PKG_VERSION")))
            }),
            timing,
            loaded: false,
            cached_sequence: None,
            etag: None,
            rejected_cache: None,
            failures: 0,
            next_attempt: None,
        }
    }

    /// 刷新一次。`have_sequence` 是 daemon 当前持有的序号（0 = 无）。
    ///
    /// 顺序：首次调用先重载并重验磁盘缓存；缓存比 daemon 新就先交付缓存；退避窗口内到此为止
    /// （不联网）；否则条件请求 FluxCloud。
    pub(crate) async fn refresh(
        &mut self,
        base_url: &str,
        have_sequence: u64,
        daemon: &dyn GuardDaemon,
    ) -> SyncOutcome {
        self.ensure_loaded().await;
        let have = match self.apply_cache(have_sequence, daemon).await {
            Ok(have) => have,
            Err(error) => return self.fail(&error),
        };
        if let Some(until) = self.next_attempt
            && Instant::now() < until
        {
            return SyncOutcome::BackedOff {
                retry_in: until.saturating_duration_since(Instant::now()),
            };
        }
        match self.fetch(base_url, have, daemon).await {
            Ok(outcome) => {
                self.failures = 0;
                self.next_attempt = None;
                outcome
            }
            Err(error) => self.fail(&error),
        }
    }

    /// 连续失败次数（成功一次清零）。
    #[cfg(test)]
    pub(crate) fn failures(&self) -> u32 {
        self.failures
    }

    #[cfg(test)]
    pub(crate) fn cached_sequence(&self) -> Option<u64> {
        self.cached_sequence
    }

    fn fail(&mut self, error: &SyncError) -> SyncOutcome {
        self.failures = self.failures.saturating_add(1);
        let retry_in = self.timing.delay(self.failures);
        self.next_attempt = Some(Instant::now() + retry_in);
        tracing::warn!(
            error = %error,
            failures = self.failures,
            retry_secs = retry_in.as_secs(),
            "BT guard cloud list refresh failed"
        );
        SyncOutcome::Failed {
            retry_in,
            error: error.to_string(),
        }
    }

    async fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        self.cached_sequence = self.read_cache().await.map(|doc| doc.sequence);
    }

    /// 读回并重新验签缓存；缺失 / 无效返回 `None`（无效文件删除，防止被篡改的缓存被反复读到）。
    /// 先查文件长度：超过信封上限的缓存不读入内存，直接删除。
    async fn read_cache(&self) -> Option<VerifiedList> {
        let path = self.dir.join(CACHE_FILE);
        let bytes = match read_bounded(&path).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                tracing::warn!("discarding oversized BT guard cloud cache");
                self.discard_cache(&path).await;
                return None;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(error = %error, "could not read BT guard cloud cache");
                return None;
            }
        };
        match text_of(bytes).and_then(|text| verify_list(text, &self.keys)) {
            Ok(doc) => Some(doc),
            Err(error) => {
                tracing::warn!(error = %error, "discarding invalid BT guard cloud cache");
                self.discard_cache(&path).await;
                None
            }
        }
    }

    async fn discard_cache(&self, path: &std::path::Path) {
        if let Err(error) = tokio::fs::remove_file(path).await {
            tracing::warn!(error = %error, "could not delete invalid BT guard cloud cache");
        }
    }

    /// 缓存比 daemon 持有的新时先交付缓存；返回交付后 daemon 持有的序号。
    async fn apply_cache(&mut self, have: u64, daemon: &dyn GuardDaemon) -> Result<u64, SyncError> {
        let Some(sequence) = self.cached_sequence else {
            return Ok(have);
        };
        if sequence <= have || self.rejected_cache == Some(sequence) {
            return Ok(have);
        }
        let Some(doc) = self.read_cache().await else {
            self.cached_sequence = None;
            self.etag = None;
            return Ok(have);
        };
        let reply = daemon
            .apply_cloud(&doc.apply)
            .await
            .map_err(SyncError::Daemon)?;
        if reply.applied {
            tracing::info!(
                sequence = doc.sequence,
                "BT guard cached cloud list applied"
            );
            Ok(doc.sequence)
        } else {
            tracing::warn!(
                sequence = doc.sequence,
                reason = reply.rejected_reason.as_deref().unwrap_or(""),
                "daemon rejected the cached BT guard cloud list"
            );
            self.rejected_cache = Some(doc.sequence);
            Ok(have)
        }
    }

    async fn fetch(
        &mut self,
        base_url: &str,
        have: u64,
        daemon: &dyn GuardDaemon,
    ) -> Result<SyncOutcome, SyncError> {
        let url = format!("{}{LIST_PATH}", base_url.trim_end_matches('/'));
        // 只有确实持有该文档时才发条件请求：否则 304 会让我们一直空手。
        let conditional = self.cached_sequence.and_then(|_| self.etag.clone());
        // 载荷由内置公钥验签：允许统一出口里的插件改写地址（失败回退原地址）。
        let plan = self.egress.plan(&url, true).await;
        let client = self
            .http
            .get(&plan.network)
            .await
            .map_err(|error| SyncError::Http(error.to_string()))?;
        let baseline = [self.cached_sequence, (have > 0).then_some(have)]
            .into_iter()
            .flatten()
            .max();
        let mut last = SyncError::Http("no attempts".to_owned());
        for attempt in plan.attempts() {
            let (text, etag) = match download(&client, attempt, conditional.as_deref()).await {
                Ok(Downloaded::NotModified) => return Ok(SyncOutcome::Unchanged),
                Ok(Downloaded::Body { text, etag }) => (text, etag),
                Err(error) => {
                    last = error;
                    continue;
                }
            };
            let doc = match verify_list(text, &self.keys) {
                Ok(doc) => doc,
                Err(error) => {
                    last = error.into();
                    continue;
                }
            };
            match baseline {
                Some(have) if doc.sequence < have => {
                    last = ListError::Rollback {
                        have,
                        got: doc.sequence,
                    }
                    .into();
                }
                Some(have) if doc.sequence == have => {
                    self.adopt(&doc, etag).await;
                    return Ok(SyncOutcome::Unchanged);
                }
                _ => return self.deliver(doc, etag, daemon).await,
            }
        }
        Err(last)
    }

    /// 序号高于已持有的文档：交给 daemon，接受后才落盘。
    async fn deliver(
        &mut self,
        doc: VerifiedList,
        etag: Option<String>,
        daemon: &dyn GuardDaemon,
    ) -> Result<SyncOutcome, SyncError> {
        let reply = daemon
            .apply_cloud(&doc.apply)
            .await
            .map_err(SyncError::Daemon)?;
        if !reply.applied {
            let reason = reply
                .rejected_reason
                .unwrap_or_else(|| "rejected by daemon".to_owned());
            tracing::warn!(sequence = doc.sequence, %reason, "daemon rejected the BT guard cloud list");
            return Ok(SyncOutcome::Rejected { reason });
        }
        let sequence = doc.sequence;
        self.adopt(&doc, etag).await;
        tracing::info!(sequence, "BT guard cloud list applied");
        Ok(SyncOutcome::Applied { sequence })
    }

    /// 把 daemon 已持有的文档记为本地基线并落盘（写盘失败只告警：daemon 自己也持久化了一份）。
    /// 缓存里已是同一序号（重启后第一次拿到同序号的 200）时只补上验证器，不重写文件。
    async fn adopt(&mut self, doc: &VerifiedList, etag: Option<String>) {
        if self.cached_sequence != Some(doc.sequence) {
            if let Err(error) = write_cache(&self.dir, &doc.apply.envelope).await {
                tracing::warn!(error = %error, "could not persist BT guard cloud cache");
            }
            self.cached_sequence = Some(doc.sequence);
            self.rejected_cache = None;
        }
        self.etag = etag;
    }
}

/// 流式累计响应体，超过信封上限即中止。错误串不含 URL（可能带查询参数）。
async fn download(
    client: &reqwest::Client,
    attempt: &EgressAttempt,
    etag: Option<&str>,
) -> Result<Downloaded, SyncError> {
    let http_error = |error: reqwest::Error| SyncError::Http(error.without_url().to_string());
    let too_large = || SyncError::List(ListError::Envelope(EnvelopeError::TooLarge));
    let mut request = client.get(&attempt.url);
    for (name, value) in &attempt.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if let Some(etag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let mut response = request.send().await.map_err(http_error)?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_MODIFIED && etag.is_some() {
        return Ok(Downloaded::NotModified);
    }
    if !status.is_success() {
        return Err(SyncError::Status(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ENVELOPE_BYTES as u64)
    {
        return Err(too_large());
    }
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(http_error)? {
        if body.len() + chunk.len() > MAX_ENVELOPE_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Downloaded::Body {
        text: text_of(body)?,
        etag,
    })
}

/// 读取缓存文件：长度（先查 metadata，再以 `take` 兜住读取期间被撑大的文件）超过信封上限返回
/// `Ok(None)`，不把超限内容读入内存。
async fn read_bounded(path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
    use tokio::io::AsyncReadExt;

    let limit = MAX_ENVELOPE_BYTES as u64;
    let file = tokio::fs::File::open(path).await?;
    if file.metadata().await?.len() > limit {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).await?;
    Ok((bytes.len() as u64 <= limit).then_some(bytes))
}

/// 先写 `cloud.json.tmp` 再 rename，崩溃时不会留下半截缓存。
async fn write_cache(dir: &std::path::Path, envelope: &str) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let temp = dir.join(format!("{CACHE_FILE}.tmp"));
    tokio::fs::write(&temp, envelope).await?;
    tokio::fs::rename(&temp, dir.join(CACHE_FILE)).await
}

#[cfg(test)]
mod tests {
    use super::RetryTiming;
    use std::time::Duration;

    #[test]
    fn production_backoff_starts_at_thirty_minutes_and_caps_at_six_hours() {
        let minutes = |failures| RetryTiming::PRODUCTION.delay(failures).as_secs() / 60;
        assert_eq!(minutes(1), 30);
        assert_eq!(minutes(2), 60);
        assert_eq!(minutes(3), 120);
        assert_eq!(minutes(4), 240);
        assert_eq!(minutes(5), 360);
        assert_eq!(minutes(6), 360);
        assert_eq!(minutes(u32::MAX), 360);
        // 防御：0 次失败不会得到比首个窗口更短的值。
        assert_eq!(minutes(0), 30);
        let custom = RetryTiming {
            first: Duration::from_millis(10),
            cap: Duration::from_millis(25),
        };
        assert_eq!(custom.delay(1), Duration::from_millis(10));
        assert_eq!(custom.delay(2), Duration::from_millis(20));
        assert_eq!(custom.delay(3), Duration::from_millis(25));
    }

    #[tokio::test]
    async fn cache_reads_are_bounded_by_the_envelope_limit() {
        use super::{MAX_ENVELOPE_BYTES, read_bounded};

        let dir = std::env::temp_dir().join(format!(
            "fluxdown_bt_guard_bounded_{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir).await.expect("dir");
        let at_limit = dir.join("at-limit");
        let over = dir.join("over");
        tokio::fs::write(&at_limit, vec![b' '; MAX_ENVELOPE_BYTES])
            .await
            .expect("write");
        tokio::fs::write(&over, vec![b' '; MAX_ENVELOPE_BYTES + 1])
            .await
            .expect("write");

        let kept = read_bounded(&at_limit).await.expect("readable");
        assert_eq!(kept.map(|bytes| bytes.len()), Some(MAX_ENVELOPE_BYTES));
        assert!(read_bounded(&over).await.expect("readable").is_none());
        let missing = read_bounded(&dir.join("missing"))
            .await
            .expect_err("missing");
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::debug!(%error, "bt guard test cleanup");
        }
    }
}
