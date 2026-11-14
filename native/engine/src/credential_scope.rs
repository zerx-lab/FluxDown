//! 浏览器 / 用户提供的请求凭据在引擎发出的每个请求上的作用域。
//!
//! 扩展交给引擎的只是「发往原始 URL 时浏览器会带的 Cookie 串」，每个 Cookie 的
//! Domain / Path / Secure 属性已经丢失；Authorization 等头同样只属于原始请求的源站。
//! 引擎以**锚点**（凭据所属的那次请求的 URL：HTTP 任务的任务地址、HLS / DASH 的用户
//! 清单地址）判定一个目标 URL 能否携带它们，重定向的每一跳、清单里的每个分片都按同一
//! 规则重新判定：
//!
//! | 凭据 | 目标可携带的条件 | 依据 |
//! |---|---|---|
//! | Cookie | 与锚点同一可注册域（公共后缀表 eTLD+1，含私有段）；锚点是 IP 或本身没有可注册域时要求同主机；锚点为 https 时目标也须 https | 浏览器按 Domain 属性匹配（RFC 6265 §5.1.3）。属性未知时取「同站」上界：不越出站点边界，`*.github.io` 等私有后缀下的租户互不相通 |
//! | Authorization / Proxy-Authorization | 与锚点同源，或同主机由 http 升级到 https（均为默认端口） | Fetch 规范跨源重定向移除 Authorization；curl ≥ 7.58 只发原主机 |
//! | Referer | 原值保持，目标为 http 而 Referer 为 https 时不发 | RFC 9110 §10.1.3；Go `net/http` 显式 Referer 跨跳保持 |

use std::borrow::Cow;

use url::{Host, Url};

/// 站点身份：有可注册域时取可注册域，否则（IP、`localhost`、公共后缀本身）取主机名。
#[derive(PartialEq, Eq)]
enum Site<'a> {
    Registrable(&'a str),
    Host(&'a str),
}

fn site(url: &Url) -> Option<Site<'_>> {
    match url.host()? {
        Host::Domain(domain) => {
            let domain = domain.strip_suffix('.').unwrap_or(domain);
            Some(match psl::domain_str(domain) {
                Some(registrable) => Site::Registrable(registrable),
                None => Site::Host(domain),
            })
        }
        Host::Ipv4(_) | Host::Ipv6(_) => url.host_str().map(Site::Host),
    }
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// 锚点捕获的 Cookie 能否随发往 `target` 的请求发出。
pub(crate) fn cookies_apply(anchor: &Url, target: &Url) -> bool {
    if !is_http(anchor) || !is_http(target) {
        return false;
    }
    // Secure 属性未知：https 锚点的 Cookie 可能是 Secure 的，绝不降级到明文。
    if anchor.scheme() == "https" && target.scheme() == "http" {
        return false;
    }
    match (site(anchor), site(target)) {
        (Some(a), Some(t)) => a == t,
        _ => false,
    }
}

/// 锚点的 Authorization / Proxy-Authorization 能否随发往 `target` 的请求发出。
pub(crate) fn authorization_applies(anchor: &Url, target: &Url) -> bool {
    if !is_http(anchor) || !is_http(target) {
        return false;
    }
    if anchor.origin() == target.origin() {
        return true;
    }
    // 同主机 http → https 升级跳转（默认端口）：凭据只会更安全，保持可用。
    anchor.scheme() == "http"
        && target.scheme() == "https"
        && anchor.port().is_none()
        && target.port().is_none()
        && anchor.host() == target.host()
}

/// Referer 能否随发往 `target` 的请求发出：只拦 https 来源 → http 目标的降级。
pub(crate) fn referer_applies(referer: &str, target: &Url) -> bool {
    let downgrade = referer
        .trim()
        .get(..6)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https:"));
    !(downgrade && target.scheme() == "http")
}

/// 两个 URL 是否同源（scheme + host + 端口）。
pub(crate) fn same_origin(a: &Url, b: &Url) -> bool {
    a.origin() == b.origin()
}

/// 名为 `name` 的凭据类请求头（只受同源约束，不随 Cookie 作用域放宽）。
pub(crate) fn is_authorization_header(name: &str) -> bool {
    !name.eq_ignore_ascii_case("cookie") && crate::task_edit::secret_header(name)
}

fn cookie_name(pair: &str) -> &str {
    pair.split_once('=').map_or(pair, |(name, _)| name).trim()
}

/// 合并捕获的 Cookie 串与本进程从响应 `Set-Cookie` 学到的 Cookie 串：同名以学到的为准
/// （服务器在本次下载中刚下发，比捕获快照新），其余保留捕获值。`learned` 为空时零拷贝。
pub(crate) fn merge_cookies<'a>(captured: &'a str, learned: Option<&str>) -> Cow<'a, str> {
    let Some(learned) = learned.map(str::trim).filter(|s| !s.is_empty()) else {
        return Cow::Borrowed(captured);
    };
    let learned_pairs = || {
        learned
            .split(';')
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
    };
    let mut merged = String::with_capacity(captured.len() + learned.len() + 2);
    let kept = captured
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let name = cookie_name(pair);
            !learned_pairs().any(|learned| cookie_name(learned) == name)
        });
    for pair in kept.chain(learned_pairs()) {
        if !merged.is_empty() {
            merged.push_str("; ");
        }
        merged.push_str(pair);
    }
    Cow::Owned(merged)
}

#[cfg(test)]
mod tests {
    use super::{authorization_applies, cookies_apply, merge_cookies, referer_applies};
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn cookies_follow_registrable_domain_like_domain_cookies() {
        let anchor = url("https://www.tiktok.com/aweme/v1/play/?video_id=1");
        assert!(cookies_apply(
            &anchor,
            &url("https://v16-webapp-prime.tiktok.com/video/x")
        ));
        assert!(cookies_apply(&anchor, &url("https://tiktok.com:8443/x")));
        assert!(cookies_apply(
            &url("https://a.example.co.uk/"),
            &url("https://b.example.co.uk/")
        ));
        // 站点边界：其他可注册域、仅共享公共后缀的域名。
        assert!(!cookies_apply(&anchor, &url("https://tiktokcdn.com/x")));
        assert!(!cookies_apply(
            &anchor,
            &url("https://evil.com/?u=tiktok.com")
        ));
        assert!(!cookies_apply(
            &url("https://a.example.co.uk/"),
            &url("https://b.other.co.uk/")
        ));
    }

    #[test]
    fn private_suffix_tenants_never_share_cookies() {
        let tenant = url("https://alice.github.io/repo/file.zip");
        assert!(cookies_apply(
            &tenant,
            &url("https://alice.github.io/other")
        ));
        assert!(!cookies_apply(
            &tenant,
            &url("https://mallory.github.io/steal")
        ));
        // 公共后缀本身没有可注册域：只认同主机。
        let suffix_host = url("https://github.io/x");
        assert!(!cookies_apply(&suffix_host, &tenant));
        assert!(!cookies_apply(
            &url("https://bucket.s3.amazonaws.com/a"),
            &url("https://other.s3.amazonaws.com/b")
        ));
    }

    #[test]
    fn hosts_without_registrable_domain_require_exact_host() {
        let ip = url("http://192.168.1.10:8080/a");
        assert!(cookies_apply(&ip, &url("http://192.168.1.10:9000/b")));
        assert!(!cookies_apply(&ip, &url("http://192.168.1.11:8080/a")));
        let local = url("http://localhost:3000/a");
        assert!(cookies_apply(&local, &url("http://localhost/b")));
        assert!(!cookies_apply(&local, &url("http://127.0.0.1:3000/a")));
        // 未登记的私有 TLD 按默认规则取最后一段为后缀：同一台 NAS 的子域互通，邻居不通。
        let nas = url("http://files.nas.lan/a");
        assert!(cookies_apply(&nas, &url("http://media.nas.lan/b")));
        assert!(!cookies_apply(&nas, &url("http://router.lan/b")));
    }

    #[test]
    fn https_cookies_never_downgrade_to_plaintext() {
        assert!(!cookies_apply(
            &url("https://example.com/a"),
            &url("http://example.com/a")
        ));
        assert!(cookies_apply(
            &url("http://example.com/a"),
            &url("https://cdn.example.com/a")
        ));
        assert!(!cookies_apply(
            &url("https://example.com/a"),
            &url("ftp://example.com/a")
        ));
    }

    #[test]
    fn authorization_stays_on_origin_except_https_upgrade() {
        let anchor = url("http://nas.example.com/share/a.iso");
        assert!(authorization_applies(
            &anchor,
            &url("http://nas.example.com/other")
        ));
        assert!(authorization_applies(
            &anchor,
            &url("https://nas.example.com/share/a.iso")
        ));
        assert!(!authorization_applies(
            &anchor,
            &url("https://nas.example.com:8443/a")
        ));
        assert!(!authorization_applies(
            &anchor,
            &url("http://cdn.example.com/a")
        ));
        assert!(!authorization_applies(
            &url("https://nas.example.com/a"),
            &url("http://nas.example.com/a")
        ));
    }

    #[test]
    fn referer_is_dropped_only_on_https_to_http() {
        let http = url("http://cdn.example.com/a");
        let https = url("https://cdn.example.com/a");
        assert!(!referer_applies("https://example.com/page", &http));
        assert!(!referer_applies("HTTPS://example.com/page", &http));
        assert!(referer_applies("https://example.com/page", &https));
        assert!(referer_applies("http://example.com/page", &http));
    }

    #[test]
    fn learned_cookies_override_captured_by_name() {
        assert_eq!(merge_cookies("a=1; b=2", None), "a=1; b=2");
        assert_eq!(merge_cookies("a=1; b=2", Some("  ")), "a=1; b=2");
        assert_eq!(merge_cookies("a=1; b=2", Some("b=9; c=3")), "a=1; b=9; c=3");
        assert_eq!(merge_cookies("", Some("c=3")), "c=3");
    }
}
