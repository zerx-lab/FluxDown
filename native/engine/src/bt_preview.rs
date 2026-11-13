//! 临时的磁力元数据解析，与下载 / 做种会话隔离。

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Session, SessionOptions};
use tokio_util::sync::CancellationToken;

use crate::bt_config::{BtConfig, is_magnet_url};
use crate::bt_downloader::{extra_trackers_for, magnet_with_trackers, normalize_magnet};
use crate::logger::log_info;

/// 在创建任何网络资源之前拒绝不支持的链接。
pub fn validate_magnet(url: &str) -> anyhow::Result<()> {
    if !is_magnet_url(url) || url.len() > 64 * 1024 {
        bail!("expected a magnet link of at most 64 KiB");
    }
    let magnet = librqbit::Magnet::parse(&normalize_magnet(url))?;
    if magnet.as_id20().is_none() && magnet.as_id32().is_none() {
        bail!("magnet link has no supported BitTorrent info hash");
    }
    Ok(())
}

/// 只取种子元数据；drop 这个 future 即关闭它私有的会话。
/// 调用方负责给操作设截止时间，并可经 `cancel` 取消。
pub async fn resolve_magnet(
    url: &str,
    config: &BtConfig,
    proxy: &crate::proxy_config::ProxyConfig,
    cancel: CancellationToken,
    guard: Option<&crate::bt_guard::BtGuardHost>,
) -> anyhow::Result<Vec<u8>> {
    validate_magnet(url)?;
    if !config.enabled {
        bail!("BitTorrent is disabled");
    }
    let lifetime = cancel.child_token();
    let _cleanup = lifetime.clone().drop_guard();
    tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("magnet preview cancelled"),
        result = async {
            let config_for_network = config.clone();
            let proxy = proxy.clone();
            // 系统代理探测与 SAM 地址解析都会阻塞，只做一次；会话候选在异步侧逐个尝试。
            let (proxy_url, i2p) = tokio::task::spawn_blocking(move || {
                let proxy_url = config_for_network.session_proxy_url(&proxy, || {
                    match crate::proxy_config::detect_system_proxy() {
                        Ok(found) => found,
                        Err(error) => {
                            log_info!("[bt] preview system proxy detection failed: {error:#}");
                            None
                        }
                    }
                });
                (proxy_url, crate::bt_downloader::i2p_options(&config_for_network))
            }).await.context("prepare magnet preview network policy")?;
            let session = open_session(config, proxy_url.as_deref(), i2p, &lifetime, guard).await?;
            let url = magnet_with_trackers(&normalize_magnet(url), &extra_trackers_for(config));
            resolve_in_session(&session, url, AddTorrentOptions::default()).await
        } => result,
    }
}

/// 临时会话的启动候选，与正式会话的降级阶梯同序：双栈 → 仅 IPv4 → 关闭 DHT
/// （tracker + PEX 仍可用）。没有可用 IPv6 的主机或 DHT 套接字绑定失败时，预览不应
/// 比正式下载更容易失败。
fn session_candidates(enable_dht: bool) -> Vec<(bool, bool)> {
    let mut candidates = Vec::with_capacity(4);
    if enable_dht {
        candidates.extend([(true, false), (true, true)]);
    }
    candidates.extend([(false, false), (false, true)]);
    candidates
}

async fn open_session(
    config: &BtConfig,
    proxy_url: Option<&str>,
    i2p: Option<librqbit::I2pOptions>,
    lifetime: &CancellationToken,
    guard: Option<&crate::bt_guard::BtGuardHost>,
) -> anyhow::Result<std::sync::Arc<Session>> {
    let mut last_error = None;
    for (dht, ipv4_only) in session_candidates(config.enable_dht) {
        // 每个候选独立的取消令牌：失败的候选立即释放它已经起的后台任务。
        let attempt = lifetime.child_token();
        let mut session_options = options(
            config,
            proxy_url,
            i2p.clone(),
            attempt.clone(),
            dht,
            ipv4_only,
        );
        session_options.peer_policy = guard.map(crate::bt_guard::BtGuardHost::policy);
        match Session::new_with_opts(PathBuf::new(), session_options).await {
            Ok(session) => return Ok(session),
            Err(error) => {
                attempt.cancel();
                log_info!(
                    "[bt] preview session candidate (dht={dht}, ipv4_only={ipv4_only}) unavailable: {error:#}"
                );
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no magnet preview session candidate")))
}

async fn resolve_in_session(
    session: &std::sync::Arc<Session>,
    url: String,
    mut options: AddTorrentOptions,
) -> anyhow::Result<Vec<u8>> {
    options.list_only = true;
    let response = session
        .add_torrent(AddTorrent::from_url(url), Some(options))
        .await
        .context("resolve magnet metadata")?;
    match response {
        AddTorrentResponse::ListOnly(result) => Ok(result.torrent_bytes.to_vec()),
        _ => bail!("metadata preview unexpectedly registered a torrent"),
    }
}

fn options(
    config: &BtConfig,
    proxy_url: Option<&str>,
    i2p: Option<librqbit::I2pOptions>,
    cancel: CancellationToken,
    dht: bool,
    ipv4_only: bool,
) -> SessionOptions {
    SessionOptions {
        dht: dht.then(|| librqbit::DhtSessionConfig {
            bootstrap_addrs: Some(crate::bt_downloader::dht_bootstrap_addrs()),
            port: Some(0),
            persistence: None,
            ..Default::default()
        }),
        persistence: None,
        fastresume: false,
        listen: None,
        ipv4_only,
        connect: Some(crate::bt_downloader::connection_options(config, proxy_url)),
        i2p,
        tracker_announce: crate::bt_downloader::tracker_announce_options(config),
        disable_web_seeds: !config.enable_webseed,
        disable_local_service_discovery: true,
        cancellation_token: Some(cancel),
        mse_mode: config.mse_mode.into(),
        peer_limit: Some(32),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{options, resolve_in_session, resolve_magnet, validate_magnet};
    use crate::bt_config::{BtConfig, BtMseMode};
    use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Session};
    use sha1::{Digest as _, Sha1};
    use tokio_util::sync::CancellationToken;

    #[test]
    fn metadata_preview_has_no_persistence_or_incoming_listener() {
        let config = BtConfig::default();
        let options = options(&config, None, None, CancellationToken::new(), true, false);
        assert!(options.persistence.is_none());
        assert!(options.dht.expect("DHT enabled").persistence.is_none());
        assert!(options.listen.is_none());
        assert!(!options.fastresume);
        assert!(options.disable_local_service_discovery);
        for url in [
            "https://example.test/a.torrent",
            "magnet:?dn=missing",
            "magnet:?xt=urn:btih:bad",
        ] {
            assert!(validate_magnet(url).is_err());
        }
    }

    #[test]
    fn preview_session_follows_the_download_session_fallback_ladder() {
        use super::session_candidates;
        assert_eq!(
            session_candidates(true),
            [(true, false), (true, true), (false, false), (false, true)]
        );
        assert_eq!(session_candidates(false), [(false, false), (false, true)]);
        let config = BtConfig::default();
        let dual = options(&config, None, None, CancellationToken::new(), true, false);
        assert!(!dual.ipv4_only);
        assert_eq!(
            dual.dht.expect("DHT enabled").bootstrap_addrs,
            Some(crate::bt_downloader::dht_bootstrap_addrs())
        );
        let fallback = options(&config, None, None, CancellationToken::new(), false, true);
        assert!(fallback.ipv4_only);
        assert!(fallback.dht.is_none());
    }

    #[test]
    fn preview_accepts_v1_v2_and_hybrid_magnets() {
        let v1 = "0123456789012345678901234567890123456789";
        let v2 = "0123456789012345678901234567890123456789012345678901234567890123";
        for url in [
            format!("magnet:?xt=urn:btih:{v1}"),
            format!("magnet:?xt=urn:btmh:1220{v2}"),
            format!("magnet:?xt=urn:btih:{v1}&xt=urn:btmh:1220{v2}"),
        ] {
            validate_magnet(&url).expect("supported infohash");
        }
    }

    #[test]
    fn preview_preserves_proxy_and_i2p_network_policy() {
        use crate::proxy_config::{ProxyConfig, ProxyType};
        let mut config = BtConfig {
            proxy_enabled: true,
            i2p_enabled: true,
            i2p_sam_host: "127.0.0.1".into(),
            i2p_sam_port: 7656,
            i2p_mixed: false,
            ..BtConfig::default()
        };
        let proxy = ProxyConfig {
            proxy_type: ProxyType::Socks5,
            host: "127.0.0.1".into(),
            port: 1080,
            ..ProxyConfig::default()
        };
        let url =
            config.session_proxy_url(&proxy, || panic!("manual proxy must not detect system"));
        for peers in [true, false] {
            config.proxy_peers = peers;
            config.announce_all_tiers = peers;
            config.announce_all_trackers = peers;
            let preview = options(
                &config,
                url.as_deref(),
                crate::bt_downloader::i2p_options(&config),
                CancellationToken::new(),
                config.enable_dht,
                false,
            );
            assert_eq!(preview.tracker_announce.announce_to_all_tiers, peers);
            assert_eq!(preview.tracker_announce.announce_to_all_trackers, peers);
            let connect = preview.connect.expect("outgoing connections");
            assert_eq!(
                connect.proxy_url.as_deref(),
                Some("socks5h://127.0.0.1:1080")
            );
            assert_eq!(connect.proxy_peer_connections, peers);
            let i2p = preview.i2p.expect("I2P configured");
            assert_eq!(i2p.sam_addr, "127.0.0.1:7656".parse().expect("SAM address"));
            assert!(!i2p.mixed_mode);
            assert!(preview.listen.is_none());
            assert!(preview.persistence.is_none());
        }
    }

    #[tokio::test]
    async fn pre_cancelled_preview_never_opens_a_session() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = resolve_magnet(
            "magnet:?xt=urn:btih:0000000000000000000000000000000000000000",
            &BtConfig::default(),
            &crate::proxy_config::ProxyConfig::default(),
            cancel,
            None,
        )
        .await;
        assert!(
            result
                .expect_err("cancelled")
                .to_string()
                .contains("cancelled")
        );
    }

    #[tokio::test]
    async fn loopback_magnet_fetches_metadata_without_registering_or_writing_payload() {
        let root = std::env::temp_dir().join(format!("fluxdown-preview-{}", uuid::Uuid::new_v4()));
        let seed_dir = root.join("seed");
        let preview_dir = root.join("must-not-be-created");
        tokio::fs::create_dir_all(&seed_dir)
            .await
            .expect("seed directory");
        tokio::fs::write(seed_dir.join("test.bin"), b"test")
            .await
            .expect("fixture data");
        let mut info = b"d6:lengthi4e4:name8:test.bin12:piece lengthi16384e6:pieces20:".to_vec();
        info.extend_from_slice(&Sha1::digest(b"test"));
        info.push(b'e');
        let magnet = format!("magnet:?xt=urn:btih:{}", hex::encode(Sha1::digest(&info)));
        let mut torrent = b"d4:info".to_vec();
        torrent.extend_from_slice(&info);
        torrent.push(b'e');
        let config = BtConfig {
            enable_dht: false,
            mse_mode: BtMseMode::Disabled,
            ..Default::default()
        };
        let cancel = CancellationToken::new();
        let _cleanup = cancel.clone().drop_guard();
        let mut seed_options = options(&config, None, None, cancel.child_token(), false, false);
        seed_options.disable_trackers = true;
        seed_options.listen = Some(librqbit::ListenerOptions {
            listen_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            mode: librqbit::ListenerMode::TcpOnly,
            enable_upnp_port_forwarding: false,
            ipv4_only: true,
            ..Default::default()
        });
        let seed = Session::new_with_opts(seed_dir, seed_options)
            .await
            .expect("loopback seed");
        let response = seed
            .add_torrent(
                AddTorrent::from_bytes(torrent),
                Some(AddTorrentOptions {
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await
            .expect("register test seed");
        let handle = match response {
            AddTorrentResponse::Added(_, handle) => handle,
            _ => panic!("new seed expected"),
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            handle.wait_until_initialized(),
        )
        .await
        .expect("seed timeout")
        .expect("seed ready");
        let mut preview_options = options(&config, None, None, cancel.child_token(), false, false);
        preview_options.disable_trackers = true;
        let preview = Session::new_with_opts(preview_dir.clone(), preview_options)
            .await
            .expect("preview session");
        let metadata = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            resolve_in_session(
                &preview,
                magnet,
                AddTorrentOptions {
                    initial_peers: Some(vec![seed.listen_addr().expect("seed listener")]),
                    ..Default::default()
                },
            ),
        )
        .await
        .expect("metadata timeout")
        .expect("resolve metadata");
        let parsed = crate::bt_downloader::probe_torrent_meta(String::new(), metadata);
        assert_eq!(parsed.name, "test.bin");
        assert_eq!(parsed.total_bytes, 4);
        assert_eq!(preview.with_torrents(|torrents| torrents.count()), 0);
        assert_eq!(seed.with_torrents(|torrents| torrents.count()), 1);
        assert!(!preview_dir.exists());
        preview.stop().await;
        seed.stop().await;
        drop(handle);
        drop(preview);
        drop(seed);
        tokio::fs::remove_file(root.join("seed/test.bin"))
            .await
            .expect("remove fixture");
        tokio::fs::remove_dir(root.join("seed"))
            .await
            .expect("remove seed dir");
        tokio::fs::remove_dir(root).await.expect("remove test dir");
    }
}
