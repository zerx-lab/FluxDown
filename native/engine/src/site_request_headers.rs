//! User-configured, origin-scoped defaults for download requests.

use reqwest::Url;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, REFERER};

use crate::downloader::{BLOCKED_REQUEST_HEADERS, OriginHeaders};

/// Host-local daemon configuration key.
pub const KEY: &str = "site_request_headers";

/// Validated rules, compiled once when the host loads or changes its settings.
#[derive(Debug, Default)]
pub struct SiteRequestHeaders(Vec<(Url, HeaderMap)>);

impl SiteRequestHeaders {
    /// Parse JSON rules, rejecting invalid origins, headers and duplicate origins.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        if raw.len() > 128 * 1024 {
            return Err(format!("{KEY}: rules exceed 128 KiB"));
        }
        let specs: Vec<OriginHeaders> =
            serde_json::from_str(raw).map_err(|_| format!("{KEY}: expected a JSON rule list"))?;
        if specs.len() > 200 {
            return Err(format!("{KEY}: at most 200 rules are allowed"));
        }
        let mut rules = Vec::with_capacity(specs.len());
        for (index, spec) in specs.into_iter().enumerate() {
            let error = |reason| format!("{KEY}[{index}]: {reason}");
            let origin = Url::parse(spec.origin.trim())
                .ok()
                .filter(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.path() == "/"
                        && url.query().is_none()
                        && url.fragment().is_none()
                        && url.username().is_empty()
                        && url.password().is_none()
                })
                .ok_or_else(|| error("expected an HTTP(S) origin without a path or credentials"))?;
            if rules.iter().any(|(prior, _)| prior == &origin) {
                return Err(error("duplicate origin"));
            }
            if spec.headers.is_empty() || spec.headers.len() > 32 {
                return Err(error("expected 1–32 headers"));
            }
            let mut headers = HeaderMap::with_capacity(spec.headers.len());
            for (name, value) in spec.headers {
                let name = HeaderName::try_from(name.as_str())
                    .map_err(|_| error("invalid header name"))?;
                if BLOCKED_REQUEST_HEADERS.contains(&name.as_str())
                    || matches!(
                        name.as_str(),
                        "cookie"
                            | "cookie2"
                            | "authorization"
                            | "proxy-authorization"
                            | "proxy-connection"
                            | "te"
                            | "trailer"
                            | "upgrade"
                            | "expect"
                    )
                {
                    return Err(error("a credential or transport header is not allowed"));
                }
                let value = value.trim();
                if name == REFERER && !valid_referer(value) {
                    return Err(error("invalid Referer URL"));
                }
                let mut value =
                    HeaderValue::from_str(value).map_err(|_| error("invalid header value"))?;
                value.set_sensitive(true);
                if headers.insert(name, value).is_some() {
                    return Err(error("duplicate header name"));
                }
            }
            rules.push((origin, headers));
        }
        Ok(Self(rules))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn headers_for(&self, url: &Url) -> Option<&HeaderMap> {
        self.0.iter().find_map(|(origin, headers)| {
            (origin.scheme() == url.scheme()
                && origin.host_str() == url.host_str()
                && origin.port() == url.port())
            .then_some(headers)
        })
    }
}

fn valid_referer(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}

#[cfg(test)]
mod tests {
    use super::SiteRequestHeaders;

    #[test]
    fn site_request_headers_reject_ambiguous_or_unsafe_rules() {
        for raw in [
            r#"[{"origin":"https://example.com/path","headers":{"X-Test":"a"}}]"#,
            r#"[{"origin":"https://user@example.com","headers":{"X-Test":"a"}}]"#,
            r#"[{"origin":"https://example.com","headers":{"CoOkIe":"sid=1"}}]"#,
            r#"[{"origin":"https://example.com","headers":{"Range":"bytes=0-3"}}]"#,
            r#"[{"origin":"https://example.com","headers":{"X-Test":"a\nb"}}]"#,
            r#"[{"origin":"https://example.com","headers":{"X-Test":"a","x-test":"b"}}]"#,
            r#"[{"origin":"https://example.com","headers":{"Referer":"file:///x"}}]"#,
            r#"[{"origin":"https://example.com","headers":{"X-Test":"a"}},{"origin":"https://EXAMPLE.com:443/","headers":{"X-Test":"b"}}]"#,
        ] {
            assert!(SiteRequestHeaders::parse(raw).is_err(), "{raw}");
        }
    }
}
