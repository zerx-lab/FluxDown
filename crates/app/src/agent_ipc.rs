//! 到本机 agent 的字节流：首选每用户 IPC（Unix domain socket / Windows Named Pipe），回环 TCP
//! 作回退。两者之上跑同一套 WebSocket + JSON-RPC。
//!
//! IPC 端点名由 bearer 所在目录推导（契约见 `fluxdown_protocol::local_service`），不占端口、
//! 不依赖端点发布文件。端点不存在 / 打不开 / 推导不一致都只表示「这条通道此刻不可用」，
//! 由调用方照常走 TCP 流程（探测、拉起 agent、重读端点文件）。

use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

/// IPC 升级请求使用的地址：只用于生成 `Host` 与请求行，不参与寻址。
pub(crate) const IPC_RPC_URL: &str = "ws://127.0.0.1/rpc";

/// 到 agent 的已连接字节流。
pub(crate) enum AgentStream {
    Tcp(TcpStream),
    Ipc(platform::Stream),
}

impl AgentStream {
    /// 传输名，仅用于日志。
    pub(crate) fn transport(&self) -> &'static str {
        match self {
            Self::Tcp(_) => "loopback TCP",
            Self::Ipc(_) => "local IPC",
        }
    }
}

impl AsyncRead for AgentStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Ipc(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for AgentStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Ipc(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Ipc(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Ipc(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// 连接 bearer 同目录的 agent IPC 端点，最多等 `timeout`；`None` = 此刻不可用（原因记 debug）。
pub(crate) async fn dial(bearer_path: &Path, timeout: Duration) -> Option<AgentStream> {
    let directory = bearer_path.parent().filter(|dir| dir.is_absolute())?;
    match tokio::time::timeout(timeout, platform::connect(directory)).await {
        Ok(Ok(stream)) => Some(AgentStream::Ipc(stream)),
        Ok(Err(error)) => {
            // 端点不存在是 agent 未运行 / 旧版 agent 的常态，不值得逐轮记录。
            if !matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) {
                log::debug!("agent IPC endpoint is not usable: {error}");
            }
            None
        }
        Err(_) => {
            log::debug!("agent IPC endpoint did not accept within {timeout:?}");
            None
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::io;
    use std::path::Path;

    use fluxdown_protocol::local_service::GATEWAY_IPC_SOCKET_FILE;

    pub(crate) type Stream = tokio::net::UnixStream;

    pub(super) async fn connect(directory: &Path) -> io::Result<Stream> {
        tokio::net::UnixStream::connect(directory.join(GATEWAY_IPC_SOCKET_FILE)).await
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::path::Path;

    use fluxdown_protocol::local_service::gateway_pipe_name;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

    pub(crate) type Stream = NamedPipeClient;

    /// 管道实例全被占用（`ERROR_PIPE_BUSY`）时按不可用返回：调用方本轮走 TCP，下一轮再试。
    pub(super) async fn connect(directory: &Path) -> io::Result<Stream> {
        ClientOptions::new().open(gateway_pipe_name(&directory.to_string_lossy()))
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::io;
    use std::path::Path;

    pub(crate) type Stream = tokio::io::DuplexStream;

    pub(super) async fn connect(_directory: &Path) -> io::Result<Stream> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no local IPC transport on this platform",
        ))
    }
}
