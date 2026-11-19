//! Device-local download proxy overrides. HTTP-family overrides are fallbacks
//! after task settings and site rules, not routes for host service requests.

use std::collections::HashMap;

use crate::proxy_config::{ProxyConfig, ProxyType};

/// BitTorrent proxy override setting.
pub const BT_KEY: &str = "proxy_protocol_bt";
/// eD2K proxy override setting.
pub const ED2K_KEY: &str = "proxy_protocol_ed2k";

pub(crate) const DIRECT_ROUTE: &str = crate::auto_proxy::route::PROTOCOL_DIRECT;
pub(crate) const PROXY_ROUTE: &str = crate::auto_proxy::route::PROTOCOL_PROXY;

pub(crate) fn is_fixed_route(label: &str) -> bool {
    matches!(label, DIRECT_ROUTE | PROXY_ROUTE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DownloadProtocol {
    Http,
    Ftp,
    Hls,
    Dash,
    Bt,
    Ed2k,
}

impl DownloadProtocol {
    const fn key(self) -> &'static str {
        match self {
            Self::Http => "proxy_protocol_http",
            Self::Ftp => "proxy_protocol_ftp",
            Self::Hls => "proxy_protocol_hls",
            Self::Dash => "proxy_protocol_dash",
            Self::Bt => BT_KEY,
            Self::Ed2k => ED2K_KEY,
        }
    }

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

const PROTOCOLS: [DownloadProtocol; 6] = [
    DownloadProtocol::Http,
    DownloadProtocol::Ftp,
    DownloadProtocol::Hls,
    DownloadProtocol::Dash,
    DownloadProtocol::Bt,
    DownloadProtocol::Ed2k,
];

/// Tests whether a daemon setting controls a protocol-specific download route.
#[must_use]
pub fn is_key(key: &str) -> bool {
    PROTOCOLS.iter().any(|protocol| protocol.key() == key)
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
    let protocol = PROTOCOLS
        .into_iter()
        .find(|protocol| protocol.key() == key)
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
pub(crate) struct ProtocolProxies {
    http: String,
    ftp: String,
    hls: String,
    dash: String,
    bt: String,
    ed2k: String,
}

impl ProtocolProxies {
    pub(crate) fn from_config(config: &HashMap<String, String>) -> Self {
        let read = |protocol: DownloadProtocol| {
            config
                .get(protocol.key())
                .map_or("", String::as_str)
                .trim()
                .to_owned()
        };
        Self {
            http: read(DownloadProtocol::Http),
            ftp: read(DownloadProtocol::Ftp),
            hls: read(DownloadProtocol::Hls),
            dash: read(DownloadProtocol::Dash),
            bt: read(DownloadProtocol::Bt),
            ed2k: read(DownloadProtocol::Ed2k),
        }
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
        match protocol {
            DownloadProtocol::Http => &self.http,
            DownloadProtocol::Ftp => &self.ftp,
            DownloadProtocol::Hls => &self.hls,
            DownloadProtocol::Dash => &self.dash,
            DownloadProtocol::Bt => &self.bt,
            DownloadProtocol::Ed2k => &self.ed2k,
        }
    }

    pub(crate) fn route(
        &self,
        protocol: DownloadProtocol,
    ) -> Result<Option<ProxyConfig>, crate::downloader::DownloadError> {
        parse(protocol, self.raw(protocol)).map_err(|error| {
            crate::downloader::DownloadError::Other(format!("{}: {error}", protocol.key()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{BT_KEY, DownloadProtocol, ED2K_KEY, PROTOCOLS, ProtocolProxies, parse, validate};
    use crate::proxy_config::ProxyMode;

    #[test]
    fn protocol_proxy_validates_supported_schemes_and_authorities() {
        for protocol in PROTOCOLS {
            assert!(parse(protocol, " \t ").expect("empty override").is_none());
            let direct = parse(protocol, " direct:// ")
                .expect("direct")
                .expect("override");
            assert_eq!(direct.mode, ProxyMode::None);
            assert!(validate(protocol.key(), "socks5h://user:p%40ss@[::1]:1080").is_ok());
            for invalid in [
                "DIRECT://",
                "socks5://relay",
                "socks5://relay:0",
                "socks5://relay:65536",
                "socks5://re lay:1080",
                "socks5://re\x07lay:1080",
                "socks5://relay:1080/extra",
            ] {
                assert!(validate(protocol.key(), invalid).is_err(), "{invalid:?}");
            }
        }
        assert!(validate("proxy_protocol_http", "https://relay:443").is_ok());
        assert!(validate(BT_KEY, "https://relay:443").is_err());
        assert!(validate(BT_KEY, "socks4://relay:1080").is_ok());
        assert!(validate(ED2K_KEY, "socks4://relay:1080").is_err());
        assert!(validate(ED2K_KEY, "http://relay:8080").is_err());
        let too_long = format!("socks5://user:{}@relay:1080", "a".repeat(2048));
        assert!(validate(BT_KEY, &too_long).is_err());
    }

    #[test]
    fn selecting_one_protocol_does_not_affect_another() {
        let overrides = ProtocolProxies::from_config(&std::collections::HashMap::from([
            (
                "proxy_protocol_http".to_owned(),
                "http://relay:8080".to_owned(),
            ),
            ("proxy_protocol_dash".to_owned(), "direct://".to_owned()),
            (ED2K_KEY.to_owned(), "socks5://relay:1080".to_owned()),
        ]));
        let http = overrides.route(DownloadProtocol::Http).unwrap().unwrap();
        assert_eq!(http.port, 8080);
        assert!(overrides.route(DownloadProtocol::Ftp).unwrap().is_none());
        let dash = overrides.route(DownloadProtocol::Dash).unwrap().unwrap();
        assert_eq!(dash.mode, ProxyMode::None);
        let ed2k = overrides.route(DownloadProtocol::Ed2k).unwrap().unwrap();
        assert_eq!(ed2k.port, 1080);
    }
}
