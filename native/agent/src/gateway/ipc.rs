//! UI Gateway 的本机 IPC 监听：Unix domain socket / Windows Named Pipe，与回环 TCP 并行服务
//! 同一个 router。
//!
//! 桌面界面优先经它连接：端点名由 `agent.token` 所在目录推导（契约见
//! `fluxdown_protocol::local_service`），不占端口、不依赖端点发布文件，也不经过网络栈
//! （回环端口被占用 / 落入系统保留区间 / 被安全软件过滤时仍然可用）。鉴权与 TCP 完全一致
//! （升级请求照常带 bearer）；对端按回环来源处理，处理器无需区分传输。
//!
//! 端点建不起来不是启动失败：记录原因后只走 TCP。

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

/// accept / 建实例瞬时失败后的退避，避免持续性错误让监听任务空转。
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(200);

/// IPC 对端在 `ConnectInfo` 里的地址：同一用户的本机进程，按回环来源处理。
const IPC_PEER: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

/// 已绑定的 IPC 端点。
pub struct IpcListener {
    inner: platform::Listener,
}

impl IpcListener {
    /// 在 `endpoint_dir`（`agent.token` 所在目录）对应的端点上监听。
    ///
    /// # Errors
    /// 目录不是绝对路径、端点已被另一个存活的进程占用，或系统拒绝创建。
    pub(crate) async fn bind(endpoint_dir: &Path) -> io::Result<Self> {
        if !endpoint_dir.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "IPC endpoint directory must be absolute",
            ));
        }
        Ok(Self {
            inner: platform::Listener::bind(endpoint_dir).await?,
        })
    }

    /// 端点的可读描述（socket 路径或管道名），仅用于日志。
    pub(crate) fn describe(&self) -> String {
        self.inner.describe()
    }
}

impl axum::serve::Listener for IpcListener {
    type Io = platform::Stream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        (self.inner.accept().await, IPC_PEER)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(IPC_PEER)
    }
}

#[cfg(unix)]
mod platform {
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    use fluxdown_protocol::local_service::GATEWAY_IPC_SOCKET_FILE;
    use tokio::net::{UnixListener, UnixStream};

    use super::ACCEPT_RETRY_DELAY;

    pub(crate) type Stream = UnixStream;

    pub(super) struct Listener {
        listener: UnixListener,
        path: PathBuf,
        owner_uid: u32,
    }

    impl Listener {
        pub(super) async fn bind(endpoint_dir: &Path) -> io::Result<Self> {
            let path = endpoint_dir.join(GATEWAY_IPC_SOCKET_FILE);
            if tokio::fs::try_exists(&path).await? {
                // 能连上 = 另一个 agent 正在服务；连不上 = 上次异常退出的残留。
                if UnixStream::connect(&path).await.is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "gateway IPC socket is already active",
                    ));
                }
                tokio::fs::remove_file(&path).await?;
            }
            let listener = UnixListener::bind(&path)?;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
            let owner_uid = tokio::fs::metadata(&path).await?.uid();
            Ok(Self {
                listener,
                path,
                owner_uid,
            })
        }

        pub(super) fn describe(&self) -> String {
            self.path.display().to_string()
        }

        pub(super) async fn accept(&mut self) -> UnixStream {
            loop {
                let stream = match self.listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        tracing::warn!(error = %error, "gateway IPC accept failed");
                        tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                        continue;
                    }
                };
                // 私有目录与 0600 之外的纵深防御：对端必须是 socket 属主（本进程用户）。
                if stream
                    .peer_cred()
                    .is_ok_and(|cred| cred.uid() == self.owner_uid)
                {
                    return stream;
                }
                tracing::warn!("gateway IPC rejected a connection from another user");
            }
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::debug!(path = %self.path.display(), error = %error, "could not remove gateway IPC socket");
                }
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::path::Path;

    use fluxdown_protocol::local_service::gateway_pipe_name;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    use super::ACCEPT_RETRY_DELAY;

    pub(crate) type Stream = NamedPipeServer;

    pub(super) struct Listener {
        name: String,
        /// 等待下一个客户端的管道实例；`None` = 上次创建失败，accept 时重建。
        next: Option<NamedPipeServer>,
    }

    impl Listener {
        pub(super) async fn bind(endpoint_dir: &Path) -> io::Result<Self> {
            let name = gateway_pipe_name(&endpoint_dir.to_string_lossy());
            // 首个实例独占名字：已有存活的服务端（另一个 agent 或抢注者）时创建失败。
            let first = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)?;
            Ok(Self {
                name,
                next: Some(first),
            })
        }

        pub(super) fn describe(&self) -> String {
            self.name.clone()
        }

        pub(super) async fn accept(&mut self) -> NamedPipeServer {
            loop {
                let server = match self.next.take() {
                    Some(server) => server,
                    None => match ServerOptions::new().create(&self.name) {
                        Ok(server) => server,
                        Err(error) => {
                            tracing::warn!(error = %error, "gateway IPC pipe instance creation failed");
                            tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                            continue;
                        }
                    },
                };
                if let Err(error) = server.connect().await {
                    tracing::warn!(error = %error, "gateway IPC pipe connect failed");
                    tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                    continue;
                }
                // 先备好下一个实例再交出已连接的：名字不出现空窗。
                match ServerOptions::new().create(&self.name) {
                    Ok(next) => self.next = Some(next),
                    Err(error) => {
                        tracing::warn!(error = %error, "gateway IPC pipe instance creation failed");
                    }
                }
                return server;
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::io;
    use std::path::Path;

    pub(crate) type Stream = tokio::io::DuplexStream;

    pub(super) struct Listener;

    impl Listener {
        pub(super) async fn bind(_endpoint_dir: &Path) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no local IPC transport on this platform",
            ))
        }

        pub(super) fn describe(&self) -> String {
            String::new()
        }

        pub(super) async fn accept(&mut self) -> tokio::io::DuplexStream {
            std::future::pending().await
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;
    use axum::serve::ListenerExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // Unix socket 路径有长度上限（macOS 104 字节）：用短目录名。
        let dir = std::env::temp_dir().join(format!(
            "fxipc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// IPC 连接经同一 router 服务，且以回环来源出现在 `ConnectInfo` 里。
    #[tokio::test]
    async fn ipc_peers_are_served_as_loopback() {
        let dir = temp_dir();
        let listener = IpcListener::bind(&dir).await.expect("bind");
        let path = dir.join(fluxdown_protocol::local_service::GATEWAY_IPC_SOCKET_FILE);
        let app = axum::Router::new().route(
            "/peer",
            axum::routing::get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
                peer.ip().to_string()
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(
                listener.tap_io(|_| {}),
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
        let mut stream = tokio::net::UnixStream::connect(&path)
            .await
            .expect("connect");
        stream
            .write_all(b"GET /peer HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("send request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("read response");
        assert!(response.ends_with("\r\n\r\n127.0.0.1"), "{response}");
        server.abort();
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    /// 存活的端点不被第二个监听者顶掉；异常退出留下的残留 socket 会被接管；正常释放后文件消失。
    #[tokio::test]
    async fn live_endpoint_is_exclusive_and_stale_socket_is_replaced() {
        let dir = temp_dir();
        let path = dir.join(fluxdown_protocol::local_service::GATEWAY_IPC_SOCKET_FILE);
        let first = IpcListener::bind(&dir).await.expect("first bind");
        let error = IpcListener::bind(&dir)
            .await
            .err()
            .expect("second bind must fail");
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        drop(first);
        assert!(!path.exists(), "socket removed when the listener drops");

        // 残留：绑定后丢弃底层监听但保留文件。
        drop(std::os::unix::net::UnixListener::bind(&path).expect("stale bind"));
        assert!(path.exists());
        let replaced = IpcListener::bind(&dir).await.expect("replace stale socket");
        assert_eq!(replaced.describe(), path.display().to_string());
        drop(replaced);
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    #[tokio::test]
    async fn relative_endpoint_directory_is_rejected() {
        let error = IpcListener::bind(Path::new("relative"))
            .await
            .err()
            .expect("relative directory must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
