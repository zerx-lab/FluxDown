use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use reqwest::redirect::Policy;
use url::{Host, Url};

use crate::egress::{Egress, client_builder};
use crate::proxy_config::{ProxyConfig, host_matches_no_proxy};
use crate::raster;

const TIMEOUT: Duration = Duration::from_secs(8);
static REQUESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

/// The configured proxy would resolve the image target outside our validation path.
#[derive(Debug, thiserror::Error)]
#[error("image proxy cannot verify the destination")]
pub struct ProxyDestinationUnverifiable;

/// Loads a bounded PNG preview from a stored RSS image URL without subscription credentials.
pub async fn load(egress: &Egress, url: &str) -> anyhow::Result<Vec<u8>> {
    let _permit = REQUESTS.try_acquire()?;
    tokio::time::timeout(TIMEOUT, async {
        let bytes = fetch(egress, Url::parse(url)?).await?;
        Ok(raster::rss_thumbnail(bytes).await?)
    })
    .await?
}

async fn fetch(egress: &Egress, mut url: Url) -> anyhow::Result<Vec<u8>> {
    for _ in 0..5 {
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow::anyhow!("missing image port"))?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none(),
            "invalid image URL"
        );
        let host = url
            .host()
            .ok_or_else(|| anyhow::anyhow!("missing image host"))?;
        ensure_public_literal(&host)?;
        ensure_direct_route(egress, &url).await?;
        let mut builder = client_builder(&ProxyConfig::default(), "")?
            .no_proxy()
            .redirect(Policy::none())
            .referer(false);
        if let Host::Domain(host) = host {
            // Use only vetted addresses and let the connector race IPv4/IPv6 as usual.
            let resolved: Vec<_> = tokio::net::lookup_host((host, port)).await?.collect();
            builder = builder.resolve_to_addrs(host, &vet_resolved(resolved)?);
        }
        let mut response = builder.build()?.get(url.clone()).send().await?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or_else(|| anyhow::anyhow!("missing image redirect location"))?;
            let next = url.join(location.to_str()?)?;
            anyhow::ensure!(
                url.scheme() != "https" || next.scheme() == "https",
                "downgraded image redirect"
            );
            url = next;
            continue;
        }
        anyhow::ensure!(
            response.status().is_success(),
            "image request returned {}",
            response.status()
        );
        anyhow::ensure!(
            response
                .content_length()
                .is_none_or(|n| n <= raster::MAX_SOURCE_BYTES as u64),
            "image too large"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                chunk.len() <= raster::MAX_SOURCE_BYTES - bytes.len(),
                "image too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        return Ok(bytes);
    }
    anyhow::bail!("too many image redirects")
}

async fn ensure_direct_route(egress: &Egress, url: &Url) -> anyhow::Result<()> {
    let endpoint = egress.plan(url.as_str(), false).await?.proxy;
    if endpoint.is_some_and(|proxy| {
        !url.host_str()
            .is_some_and(|host| host_matches_no_proxy(host, &proxy.no_proxy))
    }) {
        return Err(ProxyDestinationUnverifiable.into());
    }
    Ok(())
}

/// Keeps the public addresses of a resolved image host.
fn vet_resolved(resolved: Vec<SocketAddr>) -> anyhow::Result<Vec<SocketAddr>> {
    let any_resolved = !resolved.is_empty();
    let all_fake = resolved.iter().all(|address| is_fake_ip(address.ip()));
    let public: Vec<_> = resolved
        .into_iter()
        .filter(|address| public_ip(address.ip()))
        .collect();
    if public.is_empty() && any_resolved && all_fake {
        // TUN-mode proxies (Clash / Surge fake-ip) answer every domain from
        // 198.18.0.0/15 and pick the real destination themselves.
        return Err(ProxyDestinationUnverifiable.into());
    }
    anyhow::ensure!(!public.is_empty(), "no public image address");
    Ok(public)
}

fn is_fake_ip(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(ip) if matches!(ip.octets(), [198, 18 | 19, ..]))
}

fn ensure_public_literal(host: &Host<&str>) -> anyhow::Result<()> {
    let public = match host {
        Host::Domain(_) => true, // Checked after resolution and pinned before connecting.
        Host::Ipv4(ip) => public_ip(IpAddr::V4(*ip)),
        Host::Ipv6(ip) => public_ip(IpAddr::V6(*ip)),
    };
    anyhow::ensure!(public, "image destination is not permitted");
    Ok(())
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => {
            let [first, second, ..] = ip.segments();
            // NAT64 well-known prefix 64:ff9b::/96 (DNS64 / IPv6-only networks) wraps an
            // IPv4 target; validate that target. The local-use 64:ff9b:1::/48 stays rejected.
            if let [0x0064, 0xff9b, 0, 0, 0, 0, high, low] = ip.segments() {
                let [a, b] = high.to_be_bytes();
                let [c, d] = low.to_be_bytes();
                return public_v4(Ipv4Addr::new(a, b, c, d));
            }
            // Limit to global unicast; reject special assignments, tunneling, and documentation.
            first & 0xe000 == 0x2000
                && !(first == 0x2001 && (second <= 0x01ff || second == 0x0db8))
                && first != 0x2002
                && first != 0x3fff
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(matches!(a, 0 | 127 | 224..=255)
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_documentation()
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && matches!(c, 0 | 2))
        || (a == 198 && matches!(b, 18 | 19)))
}

#[cfg(test)]
mod tests {
    use super::{
        Egress, ProxyDestinationUnverifiable, ensure_direct_route, load, public_ip, vet_resolved,
    };
    use crate::db::Db;

    #[tokio::test]
    async fn public_image_hosts_in_the_bypass_list_keep_the_direct_route() -> anyhow::Result<()> {
        let db = Db::connect("sqlite::memory:").await?;
        for (key, value) in [
            ("proxy_mode", "manual"),
            ("proxy_host", "127.0.0.1"),
            ("proxy_port", "8123"),
            ("proxy_no_list", ".example.org"),
        ] {
            db.set_config(key, value).await?;
        }
        let egress = Egress::new(db);
        ensure_direct_route(
            &egress,
            &url::Url::parse("https://images.example.org/cover.png")?,
        )
        .await?;
        let proxy_error = load(&egress, "https://images.example.com/cover.png")
            .await
            .expect_err("an image outside the bypass list must not use the proxy");
        assert!(proxy_error.is::<ProxyDestinationUnverifiable>());
        let private_error = load(&egress, "http://127.0.0.1/image.png")
            .await
            .expect_err("private targets must still be rejected");
        assert!(!private_error.is::<ProxyDestinationUnverifiable>());
        Ok(())
    }

    #[test]
    fn accepts_public_dual_stack_but_rejects_nonpublic_ipv6() -> anyhow::Result<()> {
        for address in [
            "1.1.1.1",
            "2606:4700:4700::1111",
            // NAT64 of 1.1.1.1 (RFC 6052 well-known prefix).
            "64:ff9b::101:101",
        ] {
            assert!(public_ip(address.parse()?), "{address}");
        }
        for address in [
            "127.0.0.1",
            "100.64.1.1",
            "::1",
            "::ffff:127.0.0.1",
            "64:ff9b::7f00:1",
            "fe80::1",
            "fc00::1",
            "2001:db8::1",
            "2002::1",
            // NAT64 of 10.0.0.1, and the local-use NAT64 prefix.
            "64:ff9b::a00:1",
            "64:ff9b:1::101:101",
            "3fff::1",
        ] {
            assert!(!public_ip(address.parse()?), "{address}");
        }
        Ok(())
    }

    #[test]
    fn tun_fake_ip_resolution_reports_an_unverifiable_proxy() -> anyhow::Result<()> {
        let fake: std::net::SocketAddr = "198.18.0.7:443".parse()?;
        let public: std::net::SocketAddr = "1.1.1.1:443".parse()?;
        let private: std::net::SocketAddr = "10.0.0.1:443".parse()?;

        let error = vet_resolved(vec![fake]).expect_err("fake-ip only");
        assert!(error.is::<ProxyDestinationUnverifiable>());
        assert_eq!(vet_resolved(vec![fake, public])?, vec![public]);
        let error = vet_resolved(vec![fake, private]).expect_err("no public address");
        assert!(!error.is::<ProxyDestinationUnverifiable>());
        let error = vet_resolved(Vec::new()).expect_err("nothing resolved");
        assert!(!error.is::<ProxyDestinationUnverifiable>());
        Ok(())
    }

    #[tokio::test]
    async fn local_destinations_never_receive_an_image_request() -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let egress = Egress::new(Db::connect("sqlite::memory:").await?);
        for host in ["127.0.0.1", "localhost", "[::ffff:127.0.0.1]"] {
            let result = load(&egress, &format!("http://{host}:{port}/image.png")).await;
            assert!(result.is_err(), "{host}");
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
        Ok(())
    }
}
