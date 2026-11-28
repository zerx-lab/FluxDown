//! 进程级 HTTP client 缓存：键 = (代理指纹, 生效 UA, ignore_tls, HTTP 版本)。
//!
//! 构建一次约 70ms：批量导入的任务几乎共享同一组策略，站点规则的逐跳换 client
//! 也复用同一份缓存。容量有界（超 [`CLIENT_CACHE_CAP`] 整体清空），键空间由
//! 代理×UA×TLS 策略决定，通常只有个位数。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

use reqwest::Client;

use crate::downloader::{self, DownloadError};
use crate::proxy_config::ProxyConfig;
use crate::tls::HttpVersion;

/// 缓存容量上限；超出后整体清空重建。
const CLIENT_CACHE_CAP: usize = 64;

type CacheKey = (String, String, bool, HttpVersion);

static CACHE: LazyLock<Mutex<HashMap<CacheKey, Client>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock() -> MutexGuard<'static, HashMap<CacheKey, Client>> {
    CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 代理配置的缓存指纹：覆盖决定出口的全部字段（含鉴权与 no_proxy；不含 `rules`）。
pub(crate) fn proxy_fingerprint(proxy: &ProxyConfig) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        proxy.mode.as_str(),
        proxy.proxy_type.as_str(),
        proxy.host,
        proxy.port,
        proxy.username,
        proxy.password,
        proxy.no_proxy_list
    )
}

fn key(
    proxy: &ProxyConfig,
    user_agent: &str,
    ignore_tls_errors: bool,
    http: HttpVersion,
) -> CacheKey {
    (
        proxy_fingerprint(proxy),
        user_agent.to_owned(),
        ignore_tls_errors,
        http,
    )
}

/// 只查缓存，不构建。
pub(crate) fn lookup(
    proxy: &ProxyConfig,
    user_agent: &str,
    ignore_tls_errors: bool,
    http: HttpVersion,
) -> Option<Client> {
    lock()
        .get(&key(proxy, user_agent, ignore_tls_errors, http))
        .cloned()
}

/// 取（或构建并缓存）client。构建在锁外进行；并发未命中时可能重复构建，后写者覆盖。
/// `HttpVersion::Http1` 为普通下载 client，`Any` 见 [`downloader::build_http2_client`]。
pub(crate) fn get(
    proxy: &ProxyConfig,
    user_agent: &str,
    ignore_tls_errors: bool,
    http: HttpVersion,
) -> Result<Client, DownloadError> {
    if let Some(client) = lookup(proxy, user_agent, ignore_tls_errors, http) {
        return Ok(client);
    }
    let client = match http {
        HttpVersion::Http1 => {
            downloader::build_client_with_tls_policy(proxy, user_agent, ignore_tls_errors)?
        }
        HttpVersion::Any => downloader::build_http2_client(proxy, user_agent, ignore_tls_errors)?,
    };
    let mut cache = lock();
    if cache.len() >= CLIENT_CACHE_CAP {
        cache.clear();
    }
    cache.insert(
        key(proxy, user_agent, ignore_tls_errors, http),
        client.clone(),
    );
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_policy_reuses_client_and_ua_and_http_are_part_of_key() {
        let proxy = ProxyConfig::default();
        let ua = "client-cache-test-ua-1";
        let h1 = HttpVersion::Http1;
        assert!(lookup(&proxy, ua, false, h1).is_none());
        get(&proxy, ua, false, h1).expect("build");
        assert!(lookup(&proxy, ua, false, h1).is_some());
        assert!(lookup(&proxy, ua, true, h1).is_none());
        assert!(lookup(&proxy, ua, false, HttpVersion::Any).is_none());
        assert!(lookup(&proxy, "client-cache-test-ua-2", false, h1).is_none());
    }
}
