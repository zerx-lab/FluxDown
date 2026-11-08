//! 下载失败系统通知（仅桌面宿主）：agent 消费 daemon 的 `task.failed` `TaskNotice`，
//! 按设备本地偏好 [`NOTIFY_ON_FAIL_PREF`] 发一条系统通知。
//!
//! - `task.failed` 只在引擎自动重试**彻底放弃**后发出（重试期间静默），因此这里不会因网络抖动连发；
//! - `TaskNotice` 经 [`crate::cloud_notify::route_daemon_event`] 分流到本模块的有界通道，仍不进入
//!   UI 事件流；通道满则丢弃（通知是尽力而为的旁路效果）；
//! - 合并与发送由 [`crate::background_effects::BackgroundEffects`] 完成（与完成通知同一套防抖与文案目录）。

use fluxdown_protocol::TaskNoticeDto;
use tokio::sync::mpsc;

/// 设备本地偏好：下载失败时发系统通知（缺省开启）。设置页（`crates/settings/src/sections/notify.rs`）
/// 重复声明同名键；不在云同步目录内（每台设备各自决定是否被打扰）。
pub(crate) const NOTIFY_ON_FAIL_PREF: &str = "desktop.notify_on_fail";
pub(crate) const NOTIFY_ON_FAIL_DEFAULT: bool = true;

/// daemon 事件投影 → 失败通知的通道容量（事件稀疏；满则丢弃）。
const CHANNEL_CAPACITY: usize = 64;
/// 通知正文里失败原因的最大字符数。
const REASON_MAX_CHARS: usize = 120;

/// 一个放弃重试后失败的任务（已整理成可直接显示的形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailedTask {
    pub(crate) name: String,
    /// 失败原因摘要；引擎没给原因时为空。
    pub(crate) reason: String,
}

pub(crate) type FailureSender = mpsc::Sender<FailedTask>;
pub(crate) type FailureReceiver = mpsc::Receiver<FailedTask>;

pub(crate) fn channel() -> (FailureSender, FailureReceiver) {
    mpsc::channel(CHANNEL_CAPACITY)
}

/// 把一条 `TaskNotice` 交给失败通知；不是 `task.failed` 的事件被忽略，绝不阻塞调用方。
pub(crate) fn offer(sender: &FailureSender, notice: &TaskNoticeDto) {
    let Some(failed) = failed_task(notice) else {
        return;
    };
    if let Err(error) = sender.try_send(failed) {
        tracing::debug!(error = %error, "failure notice intake unavailable; dropped a failed task");
    }
}

fn failed_task(notice: &TaskNoticeDto) -> Option<FailedTask> {
    if notice.event != "task.failed" {
        return None;
    }
    let task = notice.task.as_ref()?;
    let name = if task.file_name.trim().is_empty() {
        // 尚未解析出文件名就失败：退回不带 userinfo / 查询串 / 片段的地址（口令与令牌常在其中）。
        // 先整体截掉查询串：`magnet:?…` 没有 `//`，`redact_urls` 认不出它。
        redact_urls(task.url.split(['?', '#']).next().unwrap_or_default(), true)
    } else {
        task.file_name.clone()
    };
    Some(FailedTask {
        name,
        reason: reason_summary(&task.error_message),
    })
}

/// 失败原因摘要：取首个非空行，超长按字符截断（不切坏多字节字符）并加省略号。
fn reason_summary(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    // reqwest 的状态错误形如 `... for url (https://host/p?token=...)`：先脱敏再截断。
    let line = redact_urls(line, false);
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(REASON_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 去掉文本里每个 URL 的 userinfo、query 与 fragment（令牌 / 签名 / 口令常在其中）；
/// `keep_path` 为假时路径也丢弃（令牌也可能内嵌在路径里），只留 `scheme://host[:port]`。
/// 只认 `scheme://` 形态；`magnet:?…` 这类没有 `//` 的地址由调用方先截掉查询串。
fn redact_urls(text: &str, keep_path: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((head, after)) = rest.split_once("://") {
        out.push_str(head);
        out.push_str("://");
        // URL 只在空白 / 引号 / 尖括号处结束（路径里的裸括号仍属 URL，否则其后的 query 会
        // 被当作普通文本保留）；reqwest 文案 `for url (https://…)` 收尾的右括号原样放回。
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>'))
            .unwrap_or(after.len());
        let (segment, tail) = after.split_at_checked(end).unwrap_or((after, ""));
        let url = segment.trim_end_matches(')');
        let closing = segment.get(url.len()..).unwrap_or_default();
        let authority_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let (authority, path) = url.split_at_checked(authority_end).unwrap_or((url, ""));
        out.push_str(
            authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host),
        );
        if keep_path {
            out.push_str(path.split(['?', '#']).next().unwrap_or_default());
        }
        out.push_str(closing);
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// 失败通知文案：单个任务标题「下载失败」、正文「文件名 + 换行 + 原因」；多个任务标题
/// 「N 个任务下载失败」、正文「最后一个文件名 等 N-1 个文件」。`text(key, count)` 按 en / zh
/// 基线键取文案并替换 `{count}`。
pub(crate) fn failure_text(
    tasks: &[FailedTask],
    text: impl Fn(&str, Option<usize>) -> String,
) -> Option<(String, String)> {
    let last = tasks.last()?;
    if tasks.len() == 1 {
        let body = if last.reason.is_empty() {
            last.name.clone()
        } else {
            format!("{}\n{}", last.name, last.reason)
        };
        return Some((text("subtitleError", None), body));
    }
    let count = tasks.len();
    Some((
        text("batchDownloadFailed", Some(count)),
        format!("{} {}", last.name, text("andMoreFiles", Some(count - 1))),
    ))
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{TaskNoticeDto, TaskNoticeTaskDto};

    use super::{FailedTask, channel, failure_text, offer, reason_summary};
    use crate::notification::english_text as english;

    fn notice(event: &str, file_name: &str, url: &str, error: &str) -> TaskNoticeDto {
        TaskNoticeDto {
            delivery_id: "d".to_owned(),
            event: event.to_owned(),
            timestamp_ms: 0,
            queue_id: String::new(),
            queue_name: String::new(),
            task: Some(TaskNoticeTaskDto {
                id: "t".to_owned(),
                file_name: file_name.to_owned(),
                url: url.to_owned(),
                error_message: error.to_owned(),
                ..TaskNoticeTaskDto::default()
            }),
        }
    }

    #[test]
    fn only_failed_events_are_offered_and_carry_name_and_reason() {
        let (sender, mut receiver) = channel();
        for event in ["task.completed", "task.paused", "task.started"] {
            offer(&sender, &notice(event, "a.bin", "https://x/a.bin", "boom"));
        }
        assert!(receiver.try_recv().is_err());

        offer(
            &sender,
            &notice("task.failed", "a.bin", "https://x/a.bin", "HTTP 404"),
        );
        assert_eq!(
            receiver.try_recv().ok(),
            Some(FailedTask {
                name: "a.bin".to_owned(),
                reason: "HTTP 404".to_owned(),
            })
        );
    }

    #[test]
    fn nameless_task_falls_back_to_url_without_query_secrets() {
        let (sender, mut receiver) = channel();
        offer(
            &sender,
            &notice("task.failed", "  ", "https://x/a.bin?token=s3cret#frag", ""),
        );
        let failed = receiver.try_recv().ok();
        assert_eq!(
            failed,
            Some(FailedTask {
                name: "https://x/a.bin".to_owned(),
                reason: String::new(),
            })
        );
    }

    #[test]
    fn urls_in_reason_and_name_lose_secrets() {
        assert_eq!(
            reason_summary(
                "HTTP status client error (403 Forbidden) for url (https://u:p@h.example:8080/a/b.bin?token=s3cret#f)"
            ),
            "HTTP status client error (403 Forbidden) for url (https://h.example:8080)"
        );
        let (sender, mut receiver) = channel();
        offer(
            &sender,
            &notice("task.failed", "", "ftp://u:p@h/dir/a.bin?sig=1", ""),
        );
        assert_eq!(
            receiver.try_recv().ok().map(|f| f.name).as_deref(),
            Some("ftp://h/dir/a.bin")
        );
        // 磁力链没有 `//`：整段查询串（含百分号编码的 tracker passkey）都不得进通知。
        offer(
            &sender,
            &notice(
                "task.failed",
                "",
                "magnet:?xt=urn:btih:abc&tr=http%3A%2F%2Ft%2Fa%3Fpasskey%3DSECRET",
                "",
            ),
        );
        assert_eq!(
            receiver.try_recv().ok().map(|f| f.name).as_deref(),
            Some("magnet:")
        );
        // 路径里的裸括号不截断 URL；多个 URL 各自脱敏。
        assert_eq!(
            reason_summary("GET http://h/a(tok)?x=SECRET failed, see https://[::1]:80/p?k=v"),
            "GET http://h failed, see https://[::1]:80"
        );
    }

    #[test]
    fn reason_keeps_first_line_and_truncates_on_char_boundary() {
        assert_eq!(reason_summary("\n  timeout \nstack"), "timeout");
        let long = "错".repeat(200);
        let summary = reason_summary(&long);
        assert_eq!(summary.chars().count(), 121);
        assert!(summary.ends_with('…'));
        assert_eq!(reason_summary(&"x".repeat(120)), "x".repeat(120));
    }

    #[test]
    fn single_and_batch_failure_text() {
        assert_eq!(failure_text(&[], english), None);
        let one = FailedTask {
            name: "a.bin".to_owned(),
            reason: "HTTP 404".to_owned(),
        };
        assert_eq!(
            failure_text(std::slice::from_ref(&one), english),
            Some(("Download Failed".to_owned(), "a.bin\nHTTP 404".to_owned()))
        );
        let bare = FailedTask {
            reason: String::new(),
            ..one.clone()
        };
        assert_eq!(
            failure_text(&[bare], english),
            Some(("Download Failed".to_owned(), "a.bin".to_owned()))
        );
        let last = FailedTask {
            name: "c.bin".to_owned(),
            ..one.clone()
        };
        assert_eq!(
            failure_text(&[one.clone(), one, last], english),
            Some((
                "3 Downloads Failed".to_owned(),
                "c.bin and 2 more".to_owned()
            ))
        );
    }
}
