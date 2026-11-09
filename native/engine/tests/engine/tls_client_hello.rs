//! 下载 client 的 TLS ClientHello（#1072）：必须带 `status_request`（OCSP stapling，ext 5）
//! 与 ALPN（ext 16；下载 client 强制 HTTP/1.1，只报 `http/1.1`）。Windows 的 schannel
//! （native-tls）两者都不发，部分 Cloudflare WAF 据此直接 403；engine 显式选 rustls，
//! 不受依赖图里其他 crate 打开的 reqwest 默认 native-tls 影响。
//!
//! 用 `--features reqwest/native-tls` 跑可在非 Windows 上模拟桌面包的 feature 合并。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fluxdown_engine::proxy_config::ProxyConfig;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

const EXT_STATUS_REQUEST: u16 = 5;
const EXT_ALPN: u16 = 16;
const STATUS_TYPE_OCSP: u8 = 1;

/// 本地监听：读第一条 TLS 记录（ClientHello）后断开，返回其握手消息体。
async fn capture_client_hello(client: reqwest::Client) -> Vec<u8> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let request = tokio::spawn(async move {
        client
            .get(format!("https://127.0.0.1:{port}/"))
            .send()
            .await
    });

    let (mut stream, _) = listener.accept().await.unwrap();
    let mut header = [0u8; 5];
    stream.read_exact(&mut header).await.unwrap();
    assert_eq!(header[0], 0x16, "首条记录必须是握手记录");
    let len = usize::from(u16::from_be_bytes([header[3], header[4]]));
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await.unwrap();
    drop(stream);

    let outcome = request.await.unwrap();
    assert!(outcome.is_err(), "服务端未完成握手，请求必须失败");
    body
}

/// 解析 ClientHello 的扩展列表：`(类型, 数据)`。
fn extensions(handshake: &[u8]) -> Vec<(u16, Vec<u8>)> {
    fn u16_at(buf: &[u8], at: usize) -> usize {
        usize::from(u16::from_be_bytes([buf[at], buf[at + 1]]))
    }
    assert_eq!(handshake[0], 1, "握手类型必须是 client_hello");
    // 握手头 4 字节 + legacy_version 2 + random 32。
    let mut at = 4 + 2 + 32;
    at += 1 + usize::from(handshake[at]); // session_id
    at += 2 + u16_at(handshake, at); // cipher_suites
    at += 1 + usize::from(handshake[at]); // compression_methods
    let end = at + 2 + u16_at(handshake, at);
    at += 2;
    let mut out = Vec::new();
    while at < end {
        let kind = u16::from_be_bytes([handshake[at], handshake[at + 1]]);
        let len = u16_at(handshake, at + 2);
        out.push((kind, handshake[at + 4..at + 4 + len].to_vec()));
        at += 4 + len;
    }
    assert_eq!(at, end, "扩展长度必须与声明一致");
    out
}

/// ALPN 扩展数据 → 协议名列表。
fn alpn_protocols(data: &[u8]) -> Vec<String> {
    let mut protocols = Vec::new();
    let mut at = 2;
    while at < data.len() {
        let len = usize::from(data[at]);
        protocols.push(String::from_utf8(data[at + 1..at + 1 + len].to_vec()).unwrap());
        at += 1 + len;
    }
    protocols
}

#[tokio::test]
async fn download_client_hello_carries_status_request_and_http11_alpn() {
    let client = fluxdown_engine::downloader::build_client(&ProxyConfig::default(), "").unwrap();
    let hello = capture_client_hello(client).await;
    let exts = extensions(&hello);

    let status = exts
        .iter()
        .find(|(kind, _)| *kind == EXT_STATUS_REQUEST)
        .map(|(_, data)| data.as_slice());
    assert_eq!(
        status.and_then(|data| data.first().copied()),
        Some(STATUS_TYPE_OCSP),
        "ClientHello 缺 status_request(OCSP)：{exts:?}"
    );

    let alpn = exts
        .iter()
        .find(|(kind, _)| *kind == EXT_ALPN)
        .map(|(_, data)| alpn_protocols(data));
    assert_eq!(
        alpn,
        Some(vec!["http/1.1".to_string()]),
        "下载 client 的 ALPN 必须只报 http/1.1"
    );
}
