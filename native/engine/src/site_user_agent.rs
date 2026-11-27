//! Host-local defaults for HTTP(S) download requests, in first-match order.

use std::sync::Arc;

use reqwest::header::HeaderValue;
use serde_json::{Map, Value};
use url::{Host, Url};

/// Daemon configuration key for the ordered JSON list.
pub const CONFIG_KEY: &str = "site_user_agent_rules";

#[derive(Debug)]
struct Rule {
    domain: String,
    include_subdomains: bool,
    user_agent: HeaderValue,
}

/// A validated configuration snapshot shared by all requests in a download run.
#[derive(Debug)]
pub struct Rules(Vec<Rule>);

impl Rules {
    /// Compile a host setting. Empty settings and empty lists have no rules.
    ///
    /// ```
    /// use fluxdown_engine::site_user_agent::Rules;
    /// assert!(matches!(Rules::parse("[]"), Ok(None)));
    /// assert!(Rules::parse("not JSON").is_err());
    /// ```
    pub fn parse(raw: &str) -> Result<Option<Arc<Self>>, String> {
        if raw.trim().is_empty() {
            return Ok(None);
        }
        if raw.len() > 512 * 1024 {
            return Err(format!("{CONFIG_KEY}: rule list is too large"));
        }
        let inputs: Vec<Map<String, Value>> = serde_json::from_str(raw).map_err(|error| {
            format!(
                "{CONFIG_KEY}: invalid rule list (line {}, column {})",
                error.line(),
                error.column()
            )
        })?;
        if inputs.len() > 200 {
            return Err(format!("{CONFIG_KEY}: at most 200 rules are allowed"));
        }
        if inputs.is_empty() {
            return Ok(None);
        }
        let rules = inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let invalid =
                    |field: &str| format!("{CONFIG_KEY}: rule {}: invalid {field}", index + 1);
                if input.len() != 3 {
                    return Err(invalid("fields"));
                }
                let domain = input
                    .get("domain")
                    .and_then(Value::as_str)
                    .and_then(normalize_domain)
                    .ok_or_else(|| invalid("domain"))?;
                let include_subdomains = input
                    .get("includeSubdomains")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("includeSubdomains"))?;
                let ua = input
                    .get("userAgent")
                    .and_then(Value::as_str)
                    .map(|ua| ua.trim_matches([' ', '\t']))
                    .filter(|ua| !ua.is_empty() && ua.len() <= 2048)
                    .ok_or_else(|| invalid("User-Agent"))?;
                let user_agent = HeaderValue::from_str(ua).map_err(|_| invalid("User-Agent"))?;
                Ok(Rule {
                    domain,
                    include_subdomains,
                    user_agent,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Some(Arc::new(Self(rules))))
    }

    pub(crate) fn for_url(&self, url: &Url) -> Option<&HeaderValue> {
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        let Host::Domain(host) = url.host()? else {
            return None;
        };
        let host = host.strip_suffix('.').unwrap_or(host);
        self.0.iter().find_map(|rule| {
            (host == rule.domain
                || (rule.include_subdomains
                    && host
                        .strip_suffix(rule.domain.as_str())
                        .is_some_and(|prefix| prefix.ends_with('.'))))
            .then_some(&rule.user_agent)
        })
    }
}

fn normalize_domain(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.len() > 1024
        || !raw.bytes().all(|byte| {
            !byte.is_ascii() || byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')
        })
    {
        return None;
    }
    let Host::Domain(mut domain) = Host::parse(raw).ok()? else {
        return None;
    };
    if domain.ends_with('.') {
        domain.pop();
    }
    if domain.len() > 253
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return None;
    }
    Some(domain)
}

#[cfg(test)]
mod tests {
    use super::Rules;
    use url::Url;

    #[test]
    fn site_user_agent_matches_dns_label_boundaries_in_order() -> Result<(), String> {
        let rules = Rules::parse(
            r#"[{"domain":"CDN.Example.test.","includeSubdomains":false,"userAgent":"Exact"},
                {"domain":"example.test","includeSubdomains":true,"userAgent":"Suffix"},
                {"domain":"例子.test","includeSubdomains":false,"userAgent":"IDN"}]"#,
        )?
        .ok_or("missing rules")?;
        for (url, expected) in [
            ("http://cdn.example.test:8000/file", Some("Exact")),
            ("https://x.cdn.example.test./", Some("Suffix")),
            ("https://example.test/", Some("Suffix")),
            ("https://例子.test/", Some("IDN")),
            ("https://x.例子.test/", None),
            ("https://notexample.test/", None),
            ("https://example.test.other.test/", None),
            ("ftp://example.test/", None),
        ] {
            let url = Url::parse(url).map_err(|error| error.to_string())?;
            assert_eq!(
                rules.for_url(&url).and_then(|ua| ua.to_str().ok()),
                expected
            );
        }
        Ok(())
    }

    #[test]
    fn site_user_agent_rejects_url_patterns_and_injected_headers() {
        for domain in [
            "*.example.test",
            "https://example.test",
            "example.test:80",
            "a..test",
        ] {
            let raw = serde_json::json!([{ "domain": domain, "includeSubdomains": false, "userAgent": "UA" }]);
            assert!(Rules::parse(&raw.to_string()).is_err(), "{domain}");
        }
        let raw = r#"[{"domain":"example.test","includeSubdomains":false,"userAgent":"UA\r\nCookie: injected"}]"#;
        assert!(Rules::parse(raw).is_err());
        assert!(Rules::parse(r#"[["example.test",false,"UA"]]"#).is_err());
        assert!(Rules::parse(r#"[{"domain":"example.test","userAgent":"UA"}]"#).is_err());
    }
}
