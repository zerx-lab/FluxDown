use base64::Engine as _;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;

use super::{AgentClientConfig, fetch_plugin_image, fetch_rss_image, protocol_error};

#[tokio::test]
async fn desktop_image_get_uses_gateway_route_bearer_and_bounded_png_body() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let address = listener.local_addr().expect("read test listener address");
    let directory = std::env::temp_dir().join(format!(
        "fluxdown-plugin-image-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos(),
    ));
    tokio::fs::create_dir_all(&directory)
        .await
        .expect("create test directory");
    let bearer_path = directory.join("agent.token");
    tokio::fs::write(&bearer_path, "test-secret\n")
        .await
        .expect("write test bearer");
    let config = AgentClientConfig {
        rpc_url: Some(format!("ws://{address}/rpc")),
        bearer_path: bearer_path.clone(),
    };
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept image request");
        for (index, expected_bearer) in ["test-secret", "updated-secret", "updated-secret"]
            .into_iter()
            .enumerate()
        {
            let mut request = Vec::new();
            loop {
                let mut buffer = [0_u8; 1024];
                let length = stream.read(&mut buffer).await.expect("read image request");
                assert_ne!(length, 0, "request ended before its headers");
                request.extend_from_slice(&buffer[..length]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).expect("request headers are UTF-8");
            let target = if index == 0 {
                "/api/web/plugin-images/s1/123e4567-e89b-42d3-a456-426614174000"
            } else {
                "/api/web/rss-images?sourceId=s%2B%26&guid=https%3A%2F%2Fexample.test%2F%3Fa%3D1%26b%3D%E5%9B%BE&index=2"
            };
            assert!(request.starts_with(&format!("GET {target} HTTP/1.1\r\n")));
            let request_headers = request.to_ascii_lowercase();
            assert!(
                request_headers.contains(&format!("authorization: bearer {expected_bearer}\r\n"))
            );
            assert!(request_headers.contains("cache-control: no-store\r\n"));
            assert!(request_headers.contains("accept: image/png\r\n"));
            assert!(!request.contains("?token="));
            if index == 2 {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 524289\r\n\r\n")
                    .await
                    .expect("write oversized RSS response headers");
                break;
            }
            let png = base64::engine::general_purpose::STANDARD
                .decode("iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAHUlEQVR4nGMQaThAU8QwasGoBaMWjFowasFQsAAAZsSoH59Hy/YAAAAASUVORK5CYII=")
                .expect("decode real PNG fixture");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n",
                png.len(),
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response headers");
            stream.write_all(&png).await.expect("write PNG body");
        }
    });

    let image = fetch_plugin_image(&config, "s1", "123e4567-e89b-42d3-a456-426614174000")
        .await
        .expect("fetch plugin image");
    assert!(image.starts_with(b"\x89PNG\r\n\x1a\n"));
    tokio::fs::write(&bearer_path, "updated-secret\n")
        .await
        .expect("update test bearer");

    let guid = "https://example.test/?a=1&b=图";
    let rss_image = fetch_rss_image(&config, "s+&", guid, 2)
        .await
        .expect("fetch RSS image");
    assert_eq!(rss_image, image);
    assert_eq!(
        fetch_rss_image(&config, "s+&", guid, 2).await,
        Err(protocol_error()),
    );
    server.await.expect("finish image test server");
    tokio::fs::remove_file(bearer_path)
        .await
        .expect("remove test bearer");
    tokio::fs::remove_dir(directory)
        .await
        .expect("remove test directory");
}
