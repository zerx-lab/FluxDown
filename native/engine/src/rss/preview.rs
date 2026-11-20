use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use reqwest::redirect::Policy;
use url::{Host, Url};

use crate::egress::{Egress, client_builder};
use crate::proxy_config::{ProxyConfig, host_matches_no_proxy};
use crate::raster;

const TIMEOUT: Duration = Duration::from_secs(8);
static REQUESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);

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
            let addresses: Vec<_> = tokio::net::lookup_host((host, port))
                .await?
                .filter(|address| public_ip(address.ip()))
                .collect();
            anyhow::ensure!(!addresses.is_empty(), "no public image address");
            builder = builder.resolve_to_addrs(host, &addresses);
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
    anyhow::ensure!(
        endpoint.is_none_or(|proxy| url
            .host_str()
            .is_some_and(|host| host_matches_no_proxy(host, &proxy.no_proxy))),
        "image proxy cannot verify the destination"
    );
    Ok(())
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
    use super::{Egress, ensure_direct_route, load, public_ip};
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
        assert!(
            ensure_direct_route(
                &egress,
                &url::Url::parse("https://images.example.com/cover.png")?
            )
            .await
            .is_err()
        );
        assert!(load(&egress, "http://127.0.0.1/image.png").await.is_err());
        Ok(())
    }

    #[test]
    fn accepts_public_dual_stack_but_rejects_nonpublic_ipv6() -> anyhow::Result<()> {
        for address in ["1.1.1.1", "2606:4700:4700::1111"] {
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
            "3fff::1",
        ] {
            assert!(!public_ip(address.parse()?), "{address}");
        }
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
