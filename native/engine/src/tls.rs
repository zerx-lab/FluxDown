//! 进程共享的 rustls 客户端配置。
//!
//! reqwest 的 rustls 后端每次 `ClientBuilder::build` 都重新读取系统根证书（macOS 经钥匙串，
//! release 实测每次约 70ms）并重建根证书库与校验器。引擎的下载 client 缓存最多 64 份，egress /
//! webhook / 插件 / CDN 解析各有自己的 client，空闲时也常驻 8 份左右根证书库。这里每种校验档位
//! 只装配一次 [`ClientConfig`]，经 `ClientBuilder::use_preconfigured_tls` 交给各 client：克隆只复制
//! `Arc`，根证书库与校验器全进程一份。
//!
//! 装配与 reqwest 0.12 rustls 分支逐项一致，ClientHello 不变（回归
//! `tests/engine/tls_client_hello.rs`）：进程已安装的默认 `CryptoProvider`（否则 ring）、全部协议
//! 版本、内置 WebPKI 根 + 系统根、SNI 开启、ALPN 按 HTTP 版本；忽略证书错误时用与 reqwest 相同
//! 签名方案表的放行校验器。会话恢复缓存每个 client 各自一份（与 reqwest 相同），不跨 client 共享。

use std::sync::{Arc, LazyLock};

use rustls::client::Resumption;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

use crate::logger::{log_info, log_warn};

/// client 说的 HTTP 版本。决定 ALPN，必须与 hyper 侧（`http1_only`）一致：只说 HTTP/1.1 的
/// client 若在 ALPN 里提供 h2，服务端选 h2 后请求必失败。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HttpVersion {
    /// 只说 HTTP/1.1（下载 client：每段一条连接，Range 语义最稳）。
    Http1,
    /// HTTP/2 优先，回落 HTTP/1.1。
    Any,
}

/// 严格校验（系统根 + 内置 WebPKI 根）的基础配置；ALPN 在取用时填。
static VERIFIED: LazyLock<Result<ClientConfig, String>> = LazyLock::new(|| {
    let roots = load_roots()?;
    Ok(builder()?
        .with_root_certificates(roots)
        .with_no_client_auth())
});

/// 放行任何证书的基础配置：只给用户对单个任务显式勾选「忽略证书错误」时使用。
static UNVERIFIED: LazyLock<Result<ClientConfig, String>> = LazyLock::new(|| {
    Ok(builder()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate))
        .with_no_client_auth())
});

/// 取共享配置的一份副本（ALPN 按 `http` 设好、会话缓存为新的一份）。
///
/// 装配失败（系统根证书库里一张有效证书都没有、却有无法解析的条目——reqwest 对此同样拒绝构建）
/// 时返回原因；调用方回退到 reqwest 自行装配，同样的错误会在 `build()` 时如实暴露。
pub(crate) fn client_config(
    http: HttpVersion,
    ignore_tls_errors: bool,
) -> Result<ClientConfig, &'static str> {
    let shared = if ignore_tls_errors {
        &*UNVERIFIED
    } else {
        &*VERIFIED
    };
    let mut config = shared.as_ref().map_err(String::as_str)?.clone();
    config.resumption = Resumption::default();
    config.alpn_protocols = match http {
        HttpVersion::Http1 => vec![b"http/1.1".to_vec()],
        HttpVersion::Any => vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    };
    Ok(config)
}

fn builder() -> Result<rustls::ConfigBuilder<ClientConfig, rustls::WantsVerifier>, String> {
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
    ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(rustls::ALL_VERSIONS)
        .map_err(|error| format!("TLS protocol versions: {error}"))
}

/// 内置 WebPKI 根 + 系统根。系统证书库常带格式陈旧、解析不了的条目，逐张跳过（与 reqwest 相同）；
/// 只有一张有效的都没有且确有坏条目时才判失败。
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
        log_warn!("[tls] reading system root certificates: {error}");
    }
    if valid == 0 && invalid > 0 {
        return Err(format!(
            "none of the {invalid} system root certificates could be parsed"
        ));
    }
    log_info!(
        "[tls] shared root store: {} anchors ({valid} system, {invalid} unparsable skipped)",
        roots.len()
    );
    Ok(roots)
}

/// 放行任何服务端证书。签名方案表与 reqwest 的同名校验器逐项一致（ClientHello 的
/// `signature_algorithms` 取自这里）。
#[derive(Debug)]
struct AcceptAnyCertificate;

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}
