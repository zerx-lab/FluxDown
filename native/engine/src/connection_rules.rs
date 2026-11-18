//! Persistent per-task connection ceilings for HTTP(S) / FTP(S) file downloads.
//!
//! Each nonempty line is `host=connections`; the first matching URL host wins.
//! A `*.example.com` pattern matches subdomains, not the apex. IP literals are
//! compared as addresses, without DNS resolution. Rules are device-local config,
//! independent of the expiring learned server policy cache.

use url::{Host, Url};

use crate::downloader::{DownloadError, DownloadParams};
use crate::logger::log_info;

const KEY: &str = "connection_rules";
const MAX_RULES: usize = 200;
const MAX_BYTES: usize = 32 * 1024;

struct Rule {
    host: Host,
    subdomains: bool,
    connections: i32,
}

impl Rule {
    fn parse(line: &str) -> Result<Self, String> {
        let (pattern, count) = line
            .split_once('=')
            .ok_or_else(|| "expected host=connections".to_owned())?;
        let connections = count
            .trim()
            .parse::<i32>()
            .ok()
            .filter(|n| (1..=crate::segment_coordinator::MAX_SEGMENTS).contains(n))
            .ok_or_else(|| "connections must be between 1 and 512".to_owned())?;
        let pattern = pattern.trim();
        let subdomains = pattern.starts_with("*.");
        let host = pattern.strip_prefix("*.").unwrap_or(pattern);
        let host = host.strip_suffix('.').unwrap_or(host);
        let host = match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(ip)) => Host::Ipv4(ip),
            Ok(std::net::IpAddr::V6(ip)) => Host::Ipv6(ip),
            Err(_) => Host::parse(host).map_err(|error| format!("invalid host: {error}"))?,
        };
        match &host {
            Host::Domain(domain) => {
                if domain.len() > 253
                    || domain.split('.').any(|label| {
                        label.is_empty()
                            || label.len() > 63
                            || label.starts_with('-')
                            || label.ends_with('-')
                            || !label
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                {
                    return Err("expected a hostname or IP address, not a URL or port".to_owned());
                }
            }
            Host::Ipv4(_) | Host::Ipv6(_) if subdomains => {
                return Err("wildcards require a domain name".to_owned());
            }
            Host::Ipv4(_) | Host::Ipv6(_) => {}
        }
        Ok(Self {
            host,
            subdomains,
            connections,
        })
    }

    fn matches(&self, host: Host<&str>) -> bool {
        match (&self.host, host) {
            (Host::Domain(pattern), Host::Domain(domain)) => {
                let domain = domain.strip_suffix('.').unwrap_or(domain);
                if self.subdomains {
                    domain
                        .strip_suffix(pattern.as_str())
                        .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1)
                } else {
                    domain == pattern
                }
            }
            (Host::Ipv4(pattern), Host::Ipv4(ip)) => *pattern == ip,
            (Host::Ipv6(pattern), Host::Ipv6(ip)) => *pattern == ip,
            _ => false,
        }
    }
}

fn visit(raw: &str, mut visit_rule: impl FnMut(Rule)) -> Result<(), String> {
    if raw.len() > MAX_BYTES {
        return Err("connection rules must not exceed 32 KiB".to_owned());
    }
    let mut count = 0;
    for (index, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        count += 1;
        if count > MAX_RULES {
            return Err(format!("at most {MAX_RULES} connection rules are allowed"));
        }
        let rule = Rule::parse(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        visit_rule(rule);
    }
    Ok(())
}

/// Validate a `connection_rules` config value; errors identify the invalid line.
///
/// ```
/// use fluxdown_engine::connection_rules::validate;
/// assert!(validate("drivers.amd.com=1\n*.example.com=4").is_ok());
/// assert!(validate("drivers.amd.com=0").is_err());
/// ```
pub fn validate(raw: &str) -> Result<(), String> {
    visit(raw, |_| {})
}

fn cap_for_url(raw: &str, url: &str) -> Result<Option<i32>, String> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let url = Url::parse(url).map_err(|error| format!("invalid download URL: {error}"))?;
    let mut cap = None;
    visit(raw, |rule| {
        if cap.is_none() && url.host().is_some_and(|host| rule.matches(host)) {
            cap = Some(rule.connections);
        }
    })?;
    Ok(cap)
}

pub(crate) fn min_cap(current: i32, other: i32) -> i32 {
    match (current > 0, other > 0) {
        (true, true) => current.min(other),
        (true, false) => current,
        (false, _) => other,
    }
}

pub(crate) async fn apply(params: &mut DownloadParams) -> Result<(), DownloadError> {
    if params.audio_url.is_some()
        || crate::hls_downloader::is_hls_url(&params.url)
        || crate::dash_downloader::is_dash_url(&params.url)
        || !["http://", "https://", "ftp://", "ftps://"]
            .iter()
            .any(|scheme| params.url.starts_with(scheme))
    {
        return Ok(());
    }
    let raw = params.db.get_config(KEY).await?.unwrap_or_default();
    if let Some(cap) = cap_for_url(&raw, &params.url).map_err(DownloadError::Other)? {
        params.connection_cap = min_cap(params.connection_cap, cap);
        // A single-connection rule must not launch parallel CDN, NIC or Auto
        // proxy probes before the data worker ceiling takes effect.
        if params.connection_cap == 1 {
            params.cdn.enabled = false;
            params.multi_nic = None;
            params.auto_proxy = None;
        }
        log_info!(
            "[connection-rules] task {} connection ceiling {}",
            params.task_id,
            params.connection_cap
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{cap_for_url, min_cap, validate};

    #[test]
    fn matches_hosts_not_url_substrings_and_first_rule_wins() {
        let raw = "\nDRIVERS.AMD.COM=1\n*.amd.com=4\namd.com=8\n";
        for (url, expected) in [
            ("https://drivers.amd.com/file.exe", Some(1)),
            ("http://drivers.amd.com:8080/file.exe", Some(1)),
            ("https://DRIVERS.AMD.COM./file.exe", Some(1)),
            ("https://cdn.drivers.amd.com/file.exe", Some(4)),
            ("https://amd.com/file.exe", Some(8)),
            ("https://notamd.com/", None),
            ("https://amd.com.evil.test/", None),
            ("https://evil.test/?url=https://drivers.amd.com", None),
        ] {
            assert_eq!(cap_for_url(raw, url).unwrap(), expected, "{url}");
        }
        assert_eq!(
            cap_for_url("*.amd.com=4\ndrivers.amd.com=1", "https://drivers.amd.com/").unwrap(),
            Some(4)
        );
    }

    #[test]
    fn matches_ipv4_ipv6_and_international_domains() {
        for (rule, url) in [
            ("192.0.2.1=2", "http://192.0.2.1:8080/f"),
            ("[2001:0db8::1]=2", "https://[2001:db8::1]/f"),
            ("2001:db8::1=2", "https://[2001:db8::1]/f"),
            ("bücher.example=2", "https://xn--bcher-kva.example/f"),
        ] {
            assert_eq!(cap_for_url(rule, url).unwrap(), Some(2));
        }
        assert_eq!(
            cap_for_url("192.0.2.1=2", "https://example.com/192.0.2.1").unwrap(),
            None
        );
    }

    #[test]
    fn rejects_invalid_rules_instead_of_silently_dropping_limits() {
        for rule in [
            "example.com",
            "=1",
            "*=1",
            "*.192.0.2.1=1",
            "example.com=0",
            "example.com=513",
            "example.com=-1",
            "example.com=auto",
            "example.com=1=2",
            "https://example.com=1",
            "example.com:443=1",
            "bad..example=1",
            "bad host.example=1",
            "-bad.example=1",
        ] {
            assert!(validate(rule).is_err(), "{rule}");
        }
        assert!(validate("example.com=512\r\n\r\nlocalhost=1").is_ok());
        assert!(validate("").is_ok());
        assert!(validate(&"example.com=1\n".repeat(201)).is_err());
        assert!(validate(&" ".repeat(32 * 1024 + 1)).is_err());
        assert!(
            validate("example.com=1\n\nbad=0")
                .unwrap_err()
                .starts_with("line 3:")
        );
    }

    #[test]
    fn ceiling_composition_never_raises_an_existing_limit() {
        for (current, other, expected) in [(0, 1, 1), (16, 1, 1), (1, 16, 1), (4, 0, 4), (0, 0, 0)]
        {
            assert_eq!(min_cap(current, other), expected);
        }
    }
}
