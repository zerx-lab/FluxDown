//! 插件审计：落盘写入器 [`AuditLog`] 与记录 `fluxdown.*` 能力调用的 [`AuditingBridge`]。
//!
//! 写入走无界 mpsc + 插件 runtime 上的后台任务：每 2 s 或攒满 100 条落一批，累计写入
//! 500 条做一次保留清理；没有事件时不触碰数据库（空闲静默）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::db::Db;
use crate::logger::log_info;

use super::runtime::{
    BridgeHttpRequest, BridgeHttpResponse, FfmpegAvailability, FfmpegOutcome, FfmpegSpec,
    PluginBridge, PluginError, PluginLogLevel, YtdlpAvailability, YtdlpOutcome, YtdlpSpec,
};

const TARGET_MAX_BYTES: usize = 512;
const DETAIL_MAX_BYTES: usize = 2048;
const BATCH_MAX_ROWS: usize = 100;
const BATCH_MAX_DELAY: Duration = Duration::from_secs(2);
const PRUNE_EVERY_ROWS: usize = 500;

/// 一条审计记录。`origin` ∈ `daemon|desktop`，`outcome` ∈ `ok|denied|error`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginAuditRow {
    pub id: i64,
    pub at_ms: i64,
    pub plugin_id: String,
    pub origin: String,
    pub action: String,
    pub target: String,
    pub outcome: String,
    pub detail: String,
}

impl PluginAuditRow {
    /// `id = 0`、`at_ms = 当前时间`；`target` / `detail` 在入队时截断。
    pub fn new(
        plugin_id: &str,
        origin: &str,
        action: &str,
        target: impl Into<String>,
        outcome: &str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: 0,
            at_ms: crate::task_activity::timestamp_ms(),
            plugin_id: plugin_id.to_string(),
            origin: origin.to_string(),
            action: action.to_string(),
            target: target.into(),
            outcome: outcome.to_string(),
            detail: detail.into(),
        }
    }
}

/// 按字节上限截断，落在字符边界。
pub(crate) fn truncate_bytes(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s
}

enum Msg {
    Row(Box<PluginAuditRow>),
    Flush(oneshot::Sender<()>),
}

/// 审计写入句柄；可 Clone，全部克隆共享同一个后台任务。
#[derive(Clone)]
pub struct AuditLog {
    tx: mpsc::UnboundedSender<Msg>,
}

impl AuditLog {
    /// 在 `handle`（插件 tokio runtime）上启动后台写入任务。
    pub fn new(db: Db, handle: &tokio::runtime::Handle) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        handle.spawn(writer_loop(db, rx));
        Self { tx }
    }

    /// 非阻塞入队；后台任务已退出时只记日志。
    pub fn record(&self, mut row: PluginAuditRow) {
        row.target = truncate_bytes(row.target, TARGET_MAX_BYTES);
        row.detail = truncate_bytes(row.detail, DETAIL_MAX_BYTES);
        if self.tx.send(Msg::Row(Box::new(row))).is_err() {
            log_info!("[plugin-audit] 写入任务已退出，审计记录被丢弃");
        }
    }

    /// 等待此前已入队的记录全部落盘（读取前调用，保证读到最新记录）。
    pub async fn flush(&self) {
        let (ack, done) = oneshot::channel();
        if self.tx.send(Msg::Flush(ack)).is_err() {
            return;
        }
        if done.await.is_err() {
            log_info!("[plugin-audit] flush 未被确认（写入任务已退出）");
        }
    }
}

async fn write_batch(db: &Db, buf: &mut Vec<PluginAuditRow>, since_prune: &mut usize) {
    if buf.is_empty() {
        return;
    }
    let rows = std::mem::take(buf);
    if let Err(error) = db.insert_plugin_audit(&rows).await {
        crate::logger::report_error("plugin-audit", "insert", &error);
        return;
    }
    *since_prune += rows.len();
    if *since_prune >= PRUNE_EVERY_ROWS {
        *since_prune = 0;
        if let Err(error) = db
            .prune_plugin_audit(crate::task_activity::timestamp_ms())
            .await
        {
            crate::logger::report_error("plugin-audit", "prune", &error);
        }
    }
}

async fn writer_loop(db: Db, mut rx: mpsc::UnboundedReceiver<Msg>) {
    let mut buf: Vec<PluginAuditRow> = Vec::new();
    let mut since_prune = 0usize;
    let mut deadline: Option<Instant> = None;
    loop {
        let msg = match deadline {
            None => rx.recv().await,
            Some(at) => match tokio::time::timeout_at(at, rx.recv()).await {
                Ok(msg) => msg,
                Err(_) => {
                    write_batch(&db, &mut buf, &mut since_prune).await;
                    deadline = None;
                    continue;
                }
            },
        };
        match msg {
            None => {
                write_batch(&db, &mut buf, &mut since_prune).await;
                return;
            }
            Some(Msg::Row(row)) => {
                if buf.is_empty() {
                    deadline = Some(Instant::now() + BATCH_MAX_DELAY);
                }
                buf.push(*row);
                if buf.len() >= BATCH_MAX_ROWS {
                    write_batch(&db, &mut buf, &mut since_prune).await;
                    deadline = None;
                }
            }
            Some(Msg::Flush(ack)) => {
                write_batch(&db, &mut buf, &mut since_prune).await;
                deadline = None;
                if ack.send(()).is_err() {
                    // 等待方已放弃（超时 / 取消），记录已落盘，无需处理。
                    log_info!("[plugin-audit] flush 等待方已离开");
                }
            }
        }
    }
}

/// `METHOD scheme://host[:port]/path`，去掉 userinfo、query、fragment。
fn fetch_target(method: &str, raw: &str) -> String {
    let method = if method.is_empty() { "GET" } else { method };
    let shown = match url::Url::parse(raw) {
        Ok(mut u) => {
            // 空 userinfo 恒可设置；失败只会发生在无 host 的 URL 上，此时本就没有 userinfo。
            if u.set_username("").is_err() || u.set_password(None).is_err() {
                log_info!("[plugin-audit] 无法去除 URL userinfo");
            }
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        }
        Err(_) => "invalid-url".to_string(),
    };
    format!("{} {}", method.to_ascii_uppercase(), shown)
}

fn argv_target(args: &[String]) -> String {
    truncate_bytes(args.join(" "), TARGET_MAX_BYTES)
}

/// 包装真实 bridge，对有副作用或会外联的 `flux.*` 调用记审计；读操作与
/// `storage` / `log` 不记录。
pub struct AuditingBridge {
    inner: Arc<dyn PluginBridge>,
    audit: AuditLog,
}

impl AuditingBridge {
    pub fn new(inner: Arc<dyn PluginBridge>, audit: AuditLog) -> Self {
        Self { inner, audit }
    }

    fn note<T>(
        &self,
        plugin_id: &str,
        action: &str,
        target: String,
        result: &Result<T, PluginError>,
        ok_detail: String,
    ) {
        let (outcome, detail) = match result {
            Ok(_) => ("ok", ok_detail),
            Err(error) => ("error", error.to_string()),
        };
        self.audit.record(PluginAuditRow::new(
            plugin_id, "daemon", action, target, outcome, detail,
        ));
    }
}

#[async_trait::async_trait]
impl PluginBridge for AuditingBridge {
    async fn http_request(
        &self,
        plugin_id: &str,
        req: BridgeHttpRequest,
    ) -> Result<BridgeHttpResponse, PluginError> {
        let target = fetch_target(&req.method, &req.url);
        let result = self.inner.http_request(plugin_id, req).await;
        let detail = result
            .as_ref()
            .map(|r| r.status.to_string())
            .unwrap_or_default();
        self.note(plugin_id, "fetch", target, &result, detail);
        result
    }

    async fn auth_get(
        &self,
        plugin_id: &str,
        auth_ref: &str,
    ) -> Result<Option<crate::auth::AuthProfile>, PluginError> {
        self.inner.auth_get(plugin_id, auth_ref).await
    }

    async fn auth_save(
        &self,
        plugin_id: &str,
        profile: crate::auth::AuthProfile,
    ) -> Result<String, PluginError> {
        let result = self.inner.auth_save(plugin_id, profile).await;
        self.note(
            plugin_id,
            "auth.save",
            String::new(),
            &result,
            String::new(),
        );
        result
    }

    async fn auth_remove(&self, plugin_id: &str, auth_ref: &str) -> Result<(), PluginError> {
        let result = self.inner.auth_remove(plugin_id, auth_ref).await;
        self.note(
            plugin_id,
            "auth.remove",
            String::new(),
            &result,
            String::new(),
        );
        result
    }

    async fn storage_get(&self, plugin_id: &str, key: &str) -> Option<String> {
        self.inner.storage_get(plugin_id, key).await
    }

    async fn storage_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: String,
    ) -> Result<(), PluginError> {
        self.inner.storage_set(plugin_id, key, value).await
    }

    fn log(&self, plugin_id: &str, level: PluginLogLevel, message: &str) {
        self.inner.log(plugin_id, level, message);
    }

    fn request_retry(&self, plugin_id: &str, task_id: &str, delay_ms: u64) {
        self.inner.request_retry(plugin_id, task_id, delay_ms);
        self.note(
            plugin_id,
            "task.retry",
            task_id.to_string(),
            &Ok::<(), PluginError>(()),
            format!("delayMs={delay_ms}"),
        );
    }

    async fn record_artifact(
        &self,
        plugin_id: &str,
        task_id: &str,
        file_name: &str,
    ) -> Result<(), PluginError> {
        let result = self
            .inner
            .record_artifact(plugin_id, task_id, file_name)
            .await;
        self.note(
            plugin_id,
            "task.artifact",
            task_id.to_string(),
            &result,
            file_name.to_string(),
        );
        result
    }

    async fn fs_write(
        &self,
        plugin_id: &str,
        name: &str,
        content: String,
    ) -> Result<(), PluginError> {
        let result = self.inner.fs_write(plugin_id, name, content).await;
        self.note(
            plugin_id,
            "fs.write",
            name.to_string(),
            &result,
            String::new(),
        );
        result
    }

    async fn fs_read(&self, plugin_id: &str, name: &str) -> Option<String> {
        self.inner.fs_read(plugin_id, name).await
    }

    async fn fs_remove(&self, plugin_id: &str, name: &str) -> Result<(), PluginError> {
        let result = self.inner.fs_remove(plugin_id, name).await;
        self.note(
            plugin_id,
            "fs.remove",
            name.to_string(),
            &result,
            String::new(),
        );
        result
    }

    async fn fs_list(&self, plugin_id: &str) -> Vec<String> {
        self.inner.fs_list(plugin_id).await
    }

    async fn ffmpeg_available(&self) -> Option<FfmpegAvailability> {
        self.inner.ffmpeg_available().await
    }

    async fn run_ffmpeg(
        &self,
        plugin_id: &str,
        jail_root: PathBuf,
        spec: FfmpegSpec,
    ) -> Result<FfmpegOutcome, PluginError> {
        let target = argv_target(&spec.args);
        let result = self.inner.run_ffmpeg(plugin_id, jail_root, spec).await;
        let detail = result
            .as_ref()
            .map(|o| format!("exit {}", o.code))
            .unwrap_or_default();
        self.note(plugin_id, "ffmpeg.run", target, &result, detail);
        result
    }

    async fn run_ffprobe(
        &self,
        plugin_id: &str,
        jail_root: PathBuf,
        spec: FfmpegSpec,
    ) -> Result<FfmpegOutcome, PluginError> {
        let target = argv_target(&spec.args);
        let result = self.inner.run_ffprobe(plugin_id, jail_root, spec).await;
        let detail = result
            .as_ref()
            .map(|o| format!("exit {}", o.code))
            .unwrap_or_default();
        self.note(plugin_id, "ffprobe.run", target, &result, detail);
        result
    }

    async fn ytdlp_available(&self) -> Option<YtdlpAvailability> {
        self.inner.ytdlp_available().await
    }

    async fn run_ytdlp(
        &self,
        plugin_id: &str,
        spec: YtdlpSpec,
    ) -> Result<YtdlpOutcome, PluginError> {
        let target = argv_target(&spec.args);
        let result = self.inner.run_ytdlp(plugin_id, spec).await;
        self.note(plugin_id, "ytdlp.run", target, &result, String::new());
        result
    }

    async fn remove_plugin_workspace(&self, plugin_id: &str) {
        self.inner.remove_plugin_workspace(plugin_id).await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    struct FakeBridge;

    #[async_trait::async_trait]
    impl PluginBridge for FakeBridge {
        async fn http_request(
            &self,
            _plugin_id: &str,
            _req: BridgeHttpRequest,
        ) -> Result<BridgeHttpResponse, PluginError> {
            Ok(BridgeHttpResponse {
                status: 204,
                headers: Default::default(),
                body: String::new(),
                truncated: false,
            })
        }
        async fn storage_get(&self, _p: &str, _k: &str) -> Option<String> {
            None
        }
        async fn storage_set(&self, _p: &str, _k: &str, _v: String) -> Result<(), PluginError> {
            Ok(())
        }
        fn log(&self, _p: &str, _l: PluginLogLevel, _m: &str) {}
        fn request_retry(&self, _p: &str, _t: &str, _d: u64) {}
    }

    async fn fixture() -> (Db, AuditLog) {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        let audit = AuditLog::new(db.clone(), &tokio::runtime::Handle::current());
        (db, audit)
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let s = "测试".repeat(10); // 3 字节/字
        let t = truncate_bytes(s, 10);
        assert_eq!(t.len(), 9);
        assert_eq!(truncate_bytes("abc".into(), 10), "abc");
    }

    #[test]
    fn fetch_target_strips_query_and_userinfo() {
        assert_eq!(
            fetch_target("get", "https://u:p@example.com:8443/a/b?token=1#x"),
            "GET https://example.com:8443/a/b"
        );
        assert_eq!(fetch_target("", "nonsense"), "GET invalid-url");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn db_roundtrip_filter_clear_and_paging() {
        let (db, _audit) = fixture().await;
        let rows: Vec<_> = (0..5)
            .map(|i| {
                PluginAuditRow::new(
                    if i % 2 == 0 { "a@x" } else { "b@x" },
                    "daemon",
                    "install",
                    format!("t{i}"),
                    "ok",
                    "",
                )
            })
            .collect();
        db.insert_plugin_audit(&rows).await.unwrap();
        let all = db.list_plugin_audit(None, None, 10).await.unwrap();
        assert_eq!(all.len(), 5);
        assert!(all[0].id > all[4].id);
        let a = db.list_plugin_audit(Some("a@x"), None, 10).await.unwrap();
        assert_eq!(a.len(), 3);
        let page = db
            .list_plugin_audit(None, Some(all[2].id), 10)
            .await
            .unwrap();
        assert_eq!(page.len(), 2);
        db.clear_plugin_audit(Some("a@x")).await.unwrap();
        assert_eq!(db.list_plugin_audit(None, None, 10).await.unwrap().len(), 2);
        db.clear_plugin_audit(None).await.unwrap();
        assert!(
            db.list_plugin_audit(None, None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prune_drops_rows_older_than_90_days() {
        let (db, _audit) = fixture().await;
        let now = crate::task_activity::timestamp_ms();
        let mut old = PluginAuditRow::new("a@x", "daemon", "install", "", "ok", "");
        old.at_ms = now - 91 * 24 * 60 * 60 * 1000;
        let fresh = PluginAuditRow::new("a@x", "daemon", "enable", "", "ok", "");
        db.insert_plugin_audit(&[old, fresh]).await.unwrap();
        db.prune_plugin_audit(now).await.unwrap();
        let left = db.list_plugin_audit(None, None, 10).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].action, "enable");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn log_truncates_and_flush_persists() {
        let (db, audit) = fixture().await;
        audit.record(PluginAuditRow::new(
            "a@x",
            "daemon",
            "fetch",
            "x".repeat(2000),
            "ok",
            "y".repeat(5000),
        ));
        audit.flush().await;
        let rows = db.list_plugin_audit(None, None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.len(), TARGET_MAX_BYTES);
        assert_eq!(rows[0].detail.len(), DETAIL_MAX_BYTES);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn log_batches_by_count_without_flush() {
        let (db, audit) = fixture().await;
        for i in 0..BATCH_MAX_ROWS {
            audit.record(PluginAuditRow::new(
                "a@x",
                "daemon",
                "x",
                format!("{i}"),
                "ok",
                "",
            ));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let rows = db.list_plugin_audit(None, None, 1000).await.unwrap();
        assert_eq!(rows.len(), BATCH_MAX_ROWS);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auditing_bridge_records_fetch_without_query() {
        let (db, audit) = fixture().await;
        let bridge = AuditingBridge::new(Arc::new(FakeBridge), audit.clone());
        let req = BridgeHttpRequest {
            url: "http://example.com/p?secret=1".into(),
            ..Default::default()
        };
        bridge.http_request("a@x", req).await.unwrap();
        audit.flush().await;
        let rows = db.list_plugin_audit(Some("a@x"), None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "fetch");
        assert_eq!(rows[0].target, "GET http://example.com/p");
        assert_eq!(rows[0].detail, "204");
        assert!(!rows[0].target.contains("secret"));
    }
}
