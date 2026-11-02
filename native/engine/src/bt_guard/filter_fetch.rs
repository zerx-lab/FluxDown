//! 过滤文件订阅（`bt_guard_filter_urls`）的拉取：仿 `tracker_subscription` 的取数范式。
//!
//! - 请求走宿主统一出口（[`crate::egress::RoutedClients`]），全局代理与站点规则与下载任务一致；
//! - 单源正文有大小上限，流式累计，超限即放弃该源；
//! - 空正文、没有任何可用条目的正文（HTML 错误页等）算该源失败：保留旧条目，只更新状态；
//! - 多源互不影响：某个源失败不阻断其余源。

use std::sync::Arc;
use std::time::Duration;

use super::{BtGuard, FilterSourceStatus, parse_filter_file};
use crate::proxy_config::ProxyConfig;

/// 单源响应上限。`MAX_FILTER_RANGES`（50 万）行按每行 ≤ 64 字节估算，留出注释余量。
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// 单源 HTTP 总超时。
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// 拉取并应用 `guard` 配置里的订阅源，返回刷新后的各源状态（与配置顺序一致）。
///
/// `max_age_secs` 为 `Some(n)` 时，`n` 秒内刚成功拉取过的源不重复拉取（会话反复建立时不该每次都
/// 下载大文件）；`None` 为强制刷新全部。没有任何源需要拉取时不建 client、不联网。
pub async fn refresh_sources(
    guard: &Arc<BtGuard>,
    proxy: &ProxyConfig,
    max_age_secs: Option<i64>,
) -> Vec<FilterSourceStatus> {
    let all_urls = guard.config().filter_url_list();
    let statuses = guard.filter_sources();
    let now = guard.now();
    let urls: Vec<String> = all_urls
        .into_iter()
        .filter(|url| {
            let Some(max_age) = max_age_secs else {
                return true;
            };
            !statuses.iter().any(|status| {
                status.url == *url
                    && status.ok
                    && status.updated_at > 0
                    && now.saturating_sub(status.updated_at) < max_age
            })
        })
        .collect();
    if urls.is_empty() {
        return statuses;
    }
    let proxy = proxy.clone();
    // `RoutedClients::new` 会同步检测系统代理，async 调用方须放到阻塞线程。
    let built = tokio::task::spawn_blocking(move || {
        crate::egress::RoutedClients::new(&proxy, "", |builder| builder.timeout(FETCH_TIMEOUT))
    })
    .await;
    let clients = match built {
        Ok(Ok(clients)) => clients,
        Ok(Err(error)) => {
            fail_all(
                guard,
                &urls,
                &format!("failed to build http client: {error:#}"),
            );
            return guard.filter_sources();
        }
        Err(error) => {
            fail_all(guard, &urls, &format!("http client task failed: {error:#}"));
            return guard.filter_sources();
        }
    };
    for url in &urls {
        match fetch_one(&clients, url).await {
            Ok(text) => apply(guard, url, text).await,
            Err(message) => {
                crate::log_warn!("[BtGuard] filter source fetch failed: {url}: {message}");
                guard.note_filter_error(url, &message);
            }
        }
    }
    guard.filter_sources()
}

fn fail_all(guard: &BtGuard, urls: &[String], message: &str) {
    crate::log_warn!("[BtGuard] filter sources not refreshed: {message}");
    for url in urls {
        guard.note_filter_error(url, message);
    }
}

/// 校验后整来源替换。解析 / 校验在阻塞线程（最大 50 万行），不占用 async worker。
async fn apply(guard: &Arc<BtGuard>, url: &str, text: String) {
    let owned = Arc::clone(guard);
    let url_owned = url.to_owned();
    let outcome = tokio::task::spawn_blocking(move || {
        validate_body(&text)?;
        Ok::<_, String>(owned.apply_filter_file(&url_owned, &text))
    })
    .await;
    match outcome {
        Ok(Ok(summary)) => crate::log_info!(
            "[BtGuard] filter source {url}: {} entries ({} wide dropped, {} invalid lines)",
            summary.entries,
            summary.dropped_wide,
            summary.invalid_lines
        ),
        Ok(Err(message)) => {
            crate::log_warn!("[BtGuard] filter source rejected: {url}: {message}");
            guard.note_filter_error(url, &message);
        }
        Err(error) => {
            let message = format!("filter parse task failed: {error:#}");
            crate::log_warn!("[BtGuard] filter source {url}: {message}");
            guard.note_filter_error(url, &message);
        }
    }
}

/// 空正文、或全是无法使用的行（无效 / 过宽 / eMule 放行级别）的正文视为失败；
/// 仅含注释的文件是合法的空订阅。
fn validate_body(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("empty response".to_owned());
    }
    let parsed = parse_filter_file(text);
    let unusable = parsed.invalid_lines + parsed.dropped_wide + parsed.skipped_by_level;
    if parsed.ranges.is_empty() && unusable > 0 {
        return Err(format!(
            "no usable entries ({} invalid lines, {} over-wide ranges, {} pass-level lines)",
            parsed.invalid_lines, parsed.dropped_wide, parsed.skipped_by_level
        ));
    }
    Ok(())
}

async fn fetch_one(clients: &crate::egress::RoutedClients, url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|error| format!("invalid url: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("unsupported scheme {:?}", parsed.scheme()));
    }
    let client = clients
        .client(url)
        .await
        .map_err(|error| format!("{error:#}"))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("{error:#}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("http status {status}"));
    }
    if let Some(length) = response.content_length()
        && length > MAX_BODY_BYTES as u64
    {
        return Err(format!("response too large ({length} bytes)"));
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("{error:#}"))?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(format!("response exceeds {MAX_BODY_BYTES} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}
