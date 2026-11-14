//! agent 外网 HTTPS client 的起点：进程共享一份 rustls 配置（非 Windows）。
//!
//! reqwest 的 rustls 后端每次 `ClientBuilder::build` 都重新读取系统根证书（macOS 经钥匙串，
//! release 实测每次约 70ms）并各自常驻一份根证书库；云端 / 更新 / 规则同步 / 私有插件 / 统计
//! 各有自己的 client。这里只装配一次 [`rustls::ClientConfig`]，经 `use_preconfigured_tls` 交给
//! 各 client：克隆只复制 `Arc`，根证书库与校验器全进程一份。装配与 reqwest 0.12 rustls 分支
//! 逐项一致（默认 `CryptoProvider` 否则 ring、全部协议版本、内置 WebPKI 根 + 系统根、SNI、
//! ALPN 按 HTTP 版本、每个 client 各自的会话缓存）。引擎侧同一做法见
//! `native/engine/src/tls.rs`（agent 不依赖引擎，且引擎还有「忽略证书错误」档）。
//!
//! Windows 沿用 reqwest 默认的 Schannel：系统证书库由 Windows 管理，不存在逐 client 加载。
//! 回环 daemon 的 client 不经这里（`tls_built_in_root_certs(false)`，根本不需要根证书）。

/// client 说的 HTTP 版本：决定 ALPN，并与 hyper 侧的 `http1_only` 保持一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpVersion {
    /// 只说 HTTP/1.1（WebSocket 升级等）。
    Http1,
    /// HTTP/2 优先，回落 HTTP/1.1。
    Any,
}

/// 外网 client 的起点。调用方不得再自行 `http1_only()`。
pub fn client_builder(http: HttpVersion) -> reqwest::ClientBuilder {
    let builder = match http {
        HttpVersion::Http1 => reqwest::Client::builder().http1_only(),
        HttpVersion::Any => reqwest::Client::builder(),
    };
    imp::with_shared_tls(builder, http)
}

#[cfg(windows)]
mod imp {
    pub(super) fn with_shared_tls(
        builder: reqwest::ClientBuilder,
        _http: super::HttpVersion,
    ) -> reqwest::ClientBuilder {
        builder
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::{Arc, LazyLock};

    use rustls::client::Resumption;
    use rustls::{ClientConfig, RootCertStore};

    use super::HttpVersion;

    static SHARED: LazyLock<Result<ClientConfig, String>> = LazyLock::new(build);

    pub(super) fn with_shared_tls(
        builder: reqwest::ClientBuilder,
        http: HttpVersion,
    ) -> reqwest::ClientBuilder {
        match &*SHARED {
            Ok(shared) => {
                let mut config = shared.clone();
                config.resumption = Resumption::default();
                config.alpn_protocols = match http {
                    HttpVersion::Http1 => vec![b"http/1.1".to_vec()],
                    HttpVersion::Any => vec![b"h2".to_vec(), b"http/1.1".to_vec()],
                };
                builder.use_preconfigured_tls(config)
            }
            Err(reason) => {
                // 系统根证书库损坏：交回 reqwest 自行装配，同样的错误在 `build()` 时如实返回。
                tracing::warn!(%reason, "shared TLS client config unavailable");
                builder
            }
        }
    }

    fn build() -> Result<ClientConfig, String> {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
        let roots = load_roots()?;
        Ok(ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(rustls::ALL_VERSIONS)
            .map_err(|error| format!("TLS protocol versions: {error}"))?
            .with_root_certificates(roots)
            .with_no_client_auth())
    }

    /// 内置 WebPKI 根 + 系统根；系统库里解析不了的条目逐张跳过（与 reqwest 相同），只有一张
    /// 有效的都没有且确有坏条目时才判失败。
    fn load_roots() -> Result<RootCertStore, String> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let native = rustls_native_certs::load_native_certs();
        let (mut valid, mut invalid) = (0_usize, 0_usize);
        for cert in native.certs {
            match roots.add(cert) {
                Ok(()) => valid += 1,
                Err(_) => invalid += 1,
            }
        }
        for error in &native.errors {
            tracing::warn!(%error, "reading system root certificates");
        }
        if valid == 0 && invalid > 0 {
            return Err(format!(
                "none of the {invalid} system root certificates could be parsed"
            ));
        }
        tracing::debug!(
            anchors = roots.len(),
            valid,
            invalid,
            "shared TLS root store loaded"
        );
        Ok(roots)
    }
}
