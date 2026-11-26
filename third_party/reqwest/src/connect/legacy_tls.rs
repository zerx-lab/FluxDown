//! FluxDown's origin-only fallback for TLS 1.2 ECDHE CBC servers.

use super::{verbose, AsyncConn, BoxError, Conn, TlsInfoFactory};
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::TokioIo;
use openssl::error::ErrorStack;
use openssl::ssl::{SslConnector, SslMethod, SslVersion};
use openssl::x509::{store::X509StoreBuilder, X509};
use std::io::{self, ErrorKind};
use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_openssl::SslStream;

pub(super) fn can_retry(error: &io::Error) -> bool {
    if error.kind() == ErrorKind::UnexpectedEof {
        return true;
    }
    matches!(
        error
            .get_ref()
            .and_then(|source| source.downcast_ref::<rustls::Error>()),
        Some(rustls::Error::AlertReceived(
            rustls::AlertDescription::HandshakeFailure
                | rustls::AlertDescription::InsufficientSecurity
        )) | Some(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::NoCipherSuitesInCommon
        ))
    )
}

fn connector() -> Result<SslConnector, ErrorStack> {
    let mut store = X509StoreBuilder::new()?;
    for der in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        store.add_cert(X509::from_der(der.as_ref())?)?;
    }
    let native = rustls_native_certs::load_native_certs();
    for error in native.errors {
        log::debug!("legacy TLS: reading system trust: {error}");
    }
    for der in native.certs {
        match X509::from_der(der.as_ref()).and_then(|cert| store.add_cert(cert)) {
            Ok(()) => (),
            Err(error) => log::debug!("legacy TLS: skipping invalid system trust anchor: {error}"),
        }
    }
    let mut tls = SslConnector::builder(SslMethod::tls_client())?;
    tls.set_cert_store(store.build());
    tls.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    tls.set_cipher_list(concat!(
        "ECDHE-RSA-AES256-GCM-SHA384:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-RSA-CHACHA20-POLY1305:",
        "ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-ECDSA-CHACHA20-POLY1305:",
        "ECDHE-RSA-AES256-SHA384:ECDHE-RSA-AES128-SHA256:",
        "ECDHE-ECDSA-AES256-SHA384:ECDHE-ECDSA-AES128-SHA256"
    ))?;
    tls.set_security_level(2);
    tls.set_alpn_protos(b"\x08http/1.1")?;
    Ok(tls.build())
}

pub(super) async fn connect<T: AsyncConn>(
    host: &str,
    transport: T,
    verbose: verbose::Wrapper,
    tls_info: bool,
) -> Result<Conn, BoxError> {
    static CONNECTOR: LazyLock<Result<SslConnector, ErrorStack>> = LazyLock::new(connector);
    let connector = CONNECTOR
        .as_ref()
        .map_err(|error| format!("cannot configure legacy TLS: {error}"))?;
    // URI hosts keep IPv6 brackets; OpenSSL's hostname verifier takes a bare IP.
    let host = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    let ssl = connector.configure()?.into_ssl(host)?;
    let mut io = SslStream::new(ssl, TokioIo::new(transport))?;
    Pin::new(&mut io).connect().await?;
    log::debug!("using verified compatibility TLS for {host}");
    Ok(Conn {
        inner: verbose.wrap(TokioIo::new(LegacyTlsConn(io))),
        is_proxy: false,
        tls_info,
    })
}

struct LegacyTlsConn<T>(SslStream<TokioIo<T>>);

impl<T: AsyncConn> Connection for LegacyTlsConn<T> {
    fn connected(&self) -> Connected {
        self.0.get_ref().inner().connected()
    }
}

impl<T: AsyncConn> TlsInfoFactory for LegacyTlsConn<T> {
    fn tls_info(&self) -> Option<crate::tls::TlsInfo> {
        let peer_certificate = self.0.ssl().peer_certificate()?.to_der().ok()?;
        Some(crate::tls::TlsInfo {
            peer_certificate: Some(peer_certificate),
        })
    }
}

impl<T: AsyncConn> AsyncRead for LegacyTlsConn<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<T: AsyncConn> AsyncWrite for LegacyTlsConn<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
