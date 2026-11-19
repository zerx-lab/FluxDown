//! Device-local download proxy overrides. HTTP-family overrides are fallbacks
//! after task settings and site rules, not routes for host service requests.

use std::collections::HashMap;

use crate::proxy_config::{ProxyConfig, ProxyType};

/// BitTorrent proxy override setting.
pub const BT_KEY: &str = "proxy_protocol_bt";
/// eD2K proxy override setting.
pub const ED2K_KEY: &str = "proxy_protocol_ed2k";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DownloadProtocol {
    Http,
    Ftp,
    Hls,
    Dash,
    Bt,
    Ed2k,
}

impl DownloadProtocol {
    pub(crate) fn for_http_family(url: &str, has_audio: bool) -> Self {
        if crate::download_manager::is_ftp_url(url) {
            Self::Ftp
        } else if crate::hls_downloader::is_hls_url(url) {
            Self::Hls
        } else if has_audio || crate::dash_downloader::is_dash_url(url) {
            Self::Dash
        } else {
            Self::Http
        }
    }
}

const KEYS: [(&str, DownloadProtocol); 6] = [
    ("proxy_protocol_http", DownloadProtocol::Http),
    ("proxy_protocol_ftp", DownloadProtocol::Ftp),
    ("proxy_protocol_hls", DownloadProtocol::Hls),
    ("proxy_protocol_dash", DownloadProtocol::Dash),
    (BT_KEY, DownloadProtocol::Bt),
    (ED2K_KEY, DownloadProtocol::Ed2k),
];

/// Tests whether a daemon setting controls a protocol-specific download route.
#[must_use]
pub fn is_key(key: &str) -> bool {
    KEYS.iter().any(|(candidate, _)| *candidate == key)
}

/// Validates a protocol proxy setting without returning credential-bearing input in errors.
/// Empty and `direct://` values are allowed on every protocol.
///
/// ```
/// use fluxdown_engine::protocol_proxy::{ED2K_KEY, validate};
/// assert!(validate(ED2K_KEY, "socks5://proxy.test:1080").is_ok());
/// assert!(validate(ED2K_KEY, "http://proxy.test:8080").is_err());
/// ```
pub fn validate(key: &str, value: &str) -> Result<(), &'static str> {
    let protocol = KEYS
        .iter()
        .find_map(|(candidate, protocol)| (*candidate == key).then_some(*protocol))
        .ok_or("unknown protocol proxy setting")?;
    parse(protocol, value).map(|_| ())
}

pub(crate) fn parse(
    protocol: DownloadProtocol,
    value: &str,
) -> Result<Option<ProxyConfig>, &'static str> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let value = value.trim();
    if value == ProxyConfig::DIRECT_SENTINEL {
        return Ok(Some(ProxyConfig::default()));
    }
    if value.len() > crate::proxy_rules::MAX_PROXY_URL_CHARS
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
        || crate::proxy_rules::check_proxy_url(value).is_err()
        || url::Url::parse(value).is_err()
    {
        return Err("expected a proxy URL with a host and explicit port, or direct://");
    }
    let proxy = ProxyConfig::from_proxy_url(value);
    match protocol {
        DownloadProtocol::Bt if proxy.proxy_type == ProxyType::Https => {
            Err("BitTorrent requires HTTP, SOCKS4, or SOCKS5")
        }
        DownloadProtocol::Ed2k if proxy.proxy_type != ProxyType::Socks5 => {
            Err("eD2K requires SOCKS5 with UDP support")
        }
        _ if proxy.host.is_empty() || proxy.port == 0 => Err("invalid proxy host or port"),
        _ => Ok(Some(proxy)),
    }
}

/// Retains invalid persisted overrides so that a task fails closed rather than going direct.
#[derive(Default)]
pub(crate) struct ProtocolProxies([String; 6]);

impl ProtocolProxies {
    pub(crate) fn from_config(config: &HashMap<String, String>) -> Self {
        Self(KEYS.map(|(key, _)| {
            config
                .get(key)
                .map_or_else(String::new, |value| value.trim().to_owned())
        }))
    }

    #[cfg(feature = "plugins")]
    pub(crate) fn has_http_family_override(&self) -> bool {
        [
            DownloadProtocol::Http,
            DownloadProtocol::Ftp,
            DownloadProtocol::Hls,
            DownloadProtocol::Dash,
        ]
        .iter()
        .any(|protocol| !self.raw(*protocol).is_empty())
    }

    pub(crate) fn raw(&self, protocol: DownloadProtocol) -> &str {
        &self.0[protocol as usize]
    }

    pub(crate) fn route(
        &self,
        protocol: DownloadProtocol,
    ) -> Result<Option<ProxyConfig>, crate::downloader::DownloadError> {
        parse(protocol, self.raw(protocol)).map_err(|error| {
            crate::downloader::DownloadError::Other(format!(
                "{}: {error}",
                KEYS[protocol as usize].0
            ))
        })
    }
}
