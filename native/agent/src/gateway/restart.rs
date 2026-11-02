//! Gateway listener lifecycle. A replacement serves the same router before the old acceptor stops.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::serve::ListenerExt;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::{AbortHandle, Id, JoinSet};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

const READINESS_TIMEOUT: Duration = Duration::from_secs(2);

type Reply<T> = oneshot::Sender<Result<T, RestartError>>;

#[derive(Debug, thiserror::Error)]
pub(super) enum RestartError {
    #[error("gateway bind failed: {0}")]
    Bind(#[source] io::Error),
    #[error("gateway restart failed: {0}")]
    Failed(String),
}

impl RestartError {
    pub(super) fn rpc_data(&self) -> fluxdown_protocol::RpcErrorData {
        use fluxdown_protocol::{ApplicationErrorCode, ErrorReason, RpcErrorData};
        tracing::error!(error = %self, "gateway listener replacement failed");
        match self {
            Self::Bind(error) if error.kind() == io::ErrorKind::AddrInUse => {
                RpcErrorData::new(ApplicationErrorCode::Conflict, false)
                    .with_reason(ErrorReason::GatewayPortInUse)
            }
            _ => RpcErrorData::new(ApplicationErrorCode::Internal, false)
                .with_reason(ErrorReason::GatewayRestartFailed),
        }
    }
}

pub(super) struct PreparedListener {
    pub previous: SocketAddr,
    pub bound: SocketAddr,
    pub endpoint_dir: Option<PathBuf>,
}

enum Command {
    Prepare {
        port: u16,
        reply: Reply<PreparedListener>,
    },
    Commit {
        reply: Reply<()>,
    },
    Rollback {
        reply: Reply<()>,
    },
}

pub(super) struct GatewayControl {
    sender: mpsc::Sender<Command>,
    receiver: Mutex<Option<mpsc::Receiver<Command>>>,
    running: AtomicBool,
    #[cfg(test)]
    reject_readiness: AtomicBool,
}

impl GatewayControl {
    pub(super) fn new() -> Self {
        let (sender, receiver) = mpsc::channel(4);
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            running: AtomicBool::new(false),
            #[cfg(test)]
            reject_readiness: AtomicBool::new(false),
        }
    }

    async fn request<T>(
        &self,
        command: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, RestartError> {
        if !self.running.load(Ordering::Acquire) {
            return Err(RestartError::Failed(
                "gateway has no running listener".to_owned(),
            ));
        }
        let (reply, response) = oneshot::channel();
        self.sender
            .send(command(reply))
            .await
            .map_err(|_| RestartError::Failed("listener owner is unavailable".to_owned()))?;
        response.await.map_err(|_| {
            RestartError::Failed("listener owner stopped before acknowledging".to_owned())
        })?
    }

    pub(super) async fn prepare(&self, port: u16) -> Result<PreparedListener, RestartError> {
        self.request(|reply| Command::Prepare { port, reply }).await
    }

    pub(super) async fn commit(&self) -> Result<(), RestartError> {
        self.request(|reply| Command::Commit { reply }).await
    }

    pub(super) async fn rollback(&self) -> Result<(), RestartError> {
        self.request(|reply| Command::Rollback { reply }).await
    }

    #[cfg(test)]
    pub(super) fn reject_next_readiness(&self) {
        self.reject_readiness.store(true, Ordering::Release);
    }

    // One owner handles commands and server exits together; splitting these phases across
    // tasks would allow an acceptor to disappear between readiness and commit.
    pub(super) async fn run(
        &self,
        listener: TcpListener,
        app: Router,
        bearer: String,
        cancel: CancellationToken,
        endpoint_dir: Option<PathBuf>,
    ) -> io::Result<()> {
        let mut commands = self
            .receiver
            .lock()
            .await
            .take()
            .ok_or_else(|| io::Error::other("gateway listener owner was already started"))?;
        let mut tasks = JoinSet::new();
        let mut active = start_listener(listener, &app, &mut tasks)?;
        self.running.store(true, Ordering::Release);
        let mut candidate: Option<RunningListener> = None;
        let outcome = loop {
            tokio::select! {
                () = cancel.cancelled() => break Ok(()),
                completed = tasks.join_next_with_id(), if !tasks.is_empty() => {
                    let (id, failure) = match completed {
                        Some(Ok((id, result))) => (id, result.err()),
                        Some(Err(error)) => (error.id(), Some(io::Error::other(error))),
                        None => break Err(io::Error::other("gateway listener tasks disappeared")),
                    };
                    if id == active.id {
                        break Err(failure.unwrap_or_else(|| io::Error::other("active gateway listener stopped unexpectedly")));
                    }
                    if candidate.as_ref().is_some_and(|prepared| prepared.id == id)
                        && let Some(mut prepared) = candidate.take()
                    {
                        prepared.stop().await;
                    }
                    if let Some(error) = failure {
                        tracing::error!(error = %error, "replacement or retired gateway server task failed");
                    }
                }
                command = commands.recv() => {
                    let Some(command) = command else { break Ok(()); };
                    match command {
                        Command::Prepare { port, reply } => {
                            let result = if candidate.is_some() {
                                Err(RestartError::Failed("a listener replacement is already prepared".to_owned()))
                            } else {
                                let address = SocketAddr::new(active.bound.ip(), port);
                                match crate::runtime::bind_gateway_socket(address) {
                                    Err(error) => Err(RestartError::Bind(error)),
                                    Ok(listener) => match start_listener(listener, &app, &mut tasks) {
                                        Err(error) => Err(RestartError::Failed(error.to_string())),
                                        Ok(mut prepared) => {
                                            #[cfg(test)]
                                            let probe_bearer = if self.reject_readiness.swap(false, Ordering::AcqRel) {
                                                "invalid-readiness-bearer"
                                            } else { &bearer };
                                            #[cfg(not(test))]
                                            let probe_bearer = &bearer;
                                            match probe(prepared.bound, probe_bearer).await {
                                                Ok(()) => {
                                                    let result = PreparedListener {
                                                        previous: active.bound,
                                                        bound: prepared.bound,
                                                        endpoint_dir: endpoint_dir.clone(),
                                                    };
                                                    candidate = Some(prepared);
                                                    Ok(result)
                                                }
                                                Err(error) => {
                                                    prepared.stop().await;
                                                    Err(RestartError::Failed(error.to_string()))
                                                }
                                            }
                                        }
                                    },
                                }
                            };
                            if reply.send(result).is_err() {
                                if let Some(mut prepared) = candidate.take() { prepared.stop().await; }
                                tracing::debug!("gateway prepare caller closed before acknowledgement");
                            }
                        }
                        Command::Commit { reply } => {
                            let result = match candidate.take() {
                                Some(prepared) => {
                                    if prepared.task.is_finished() {
                                        let mut prepared = prepared;
                                        prepared.stop().await;
                                        if reply.send(Err(RestartError::Failed("prepared gateway listener stopped".to_owned()))).is_err() {
                                            tracing::debug!("gateway commit caller closed before failed acknowledgement");
                                        }
                                        continue;
                                    }
                                    // Drop acknowledgement proves the old TCP acceptor is gone. Its
                                    // upgraded official sockets retain only the global cancel token.
                                    active.stop().await;
                                    active = prepared;
                                    Ok(())
                                }
                                None => Err(RestartError::Failed("no prepared gateway listener".to_owned())),
                            };
                            if reply.send(result).is_err() {
                                tracing::debug!("gateway commit caller closed before acknowledgement");
                            }
                        }
                        Command::Rollback { reply } => {
                            if let Some(mut prepared) = candidate.take() { prepared.stop().await; }
                            if reply.send(Ok(())).is_err() {
                                tracing::debug!("gateway rollback caller closed before acknowledgement");
                            }
                        }
                    }
                }
            }
        };
        self.running.store(false, Ordering::Release);
        active.stop().await;
        if let Some(mut prepared) = candidate {
            prepared.stop().await;
        }
        // Global shutdown also ends official WS sessions, unlike a listener replacement.
        cancel.cancel();
        tasks.abort_all();
        let mut outcome = outcome;
        while let Some(completed) = tasks.join_next().await {
            let error = match completed {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(error) if error.is_cancelled() => None,
                Err(error) => Some(io::Error::other(error)),
            };
            if let Some(error) = error {
                if outcome.is_ok() {
                    outcome = Err(error);
                } else {
                    tracing::error!(error = %error, "gateway server task failed during shutdown");
                }
            }
        }
        outcome
    }
}

struct RunningListener {
    bound: SocketAddr,
    id: Id,
    stop: CancellationToken,
    task: AbortHandle,
    stopped: Option<oneshot::Receiver<()>>,
}

impl RunningListener {
    async fn stop(&mut self) {
        self.stop.cancel();
        if let Some(stopped) = self.stopped.take()
            && stopped.await.is_err()
        {
            // TrackedListener always acknowledges its drop; a closed receiver means its task
            // was dropped before polling. The listener has still been dropped in either case.
            tracing::debug!(address = %self.bound, "gateway listener task dropped before stop acknowledgement");
        }
    }
}

struct TrackedListener {
    // Fields drop in declaration order: acknowledge only after the TCP listener closes.
    listener: TcpListener,
    _stopped: ListenerStopped,
}

struct ListenerStopped(Option<oneshot::Sender<()>>);

impl axum::serve::Listener for TrackedListener {
    type Io = TcpStream;
    type Addr = SocketAddr;

    /// 双栈 socket 把 IPv4 对端报成 `::ffff:a.b.c.d`；还原成 IPv4，回环判定、按来源限流与
    /// 反代信任名单都只认规范形式。
    async fn accept(&mut self) -> (TcpStream, SocketAddr) {
        let (stream, peer) = axum::serve::Listener::accept(&mut self.listener).await;
        (
            stream,
            SocketAddr::new(peer.ip().to_canonical(), peer.port()),
        )
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

impl Drop for ListenerStopped {
    fn drop(&mut self) {
        if let Some(stopped) = self.0.take()
            && stopped.send(()).is_err()
        {
            tracing::trace!("gateway listener owner no longer waits for stop acknowledgement");
        }
    }
}

fn start_listener(
    listener: TcpListener,
    app: &Router,
    tasks: &mut JoinSet<io::Result<()>>,
) -> io::Result<RunningListener> {
    let bound = listener.local_addr()?;
    let stop = CancellationToken::new();
    let (stopped_tx, stopped) = oneshot::channel();
    let listener = TrackedListener {
        listener,
        _stopped: ListenerStopped(Some(stopped_tx)),
    };
    // Axum's generic SocketAddr Connected implementation is exposed through TapIo.
    // The empty tap is only its zero-cost metadata adapter for our drop-tracked listener.
    let listener = listener.tap_io(|_| {});
    let app = app.clone();
    let shutdown = stop.clone();
    let task = tasks.spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await
    });
    Ok(RunningListener {
        bound,
        id: task.id(),
        task,
        stop,
        stopped: Some(stopped),
    })
}

/// One bounded authenticated handshake against the replacement itself; no TCP-only readiness.
pub(super) async fn probe(bound: SocketAddr, bearer: &str) -> io::Result<()> {
    tokio::time::timeout(READINESS_TIMEOUT, async {
        let address = crate::state::gateway_client_address(bound);
        let mut request = format!("ws://{address}/rpc")
            .into_client_request()
            .map_err(io::Error::other)?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            format!("Bearer {bearer}")
                .parse()
                .map_err(io::Error::other)?,
        );
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(io::Error::other)?;
        let hello = fluxdown_protocol::ClientHello {
            client_name: "fluxdown-agent-readiness".to_owned(),
            client_version: fluxdown_protocol::APP_VERSION.to_owned(),
            min_protocol_version: fluxdown_protocol::MIN_PROTOCOL_VERSION,
            max_protocol_version: fluxdown_protocol::PROTOCOL_VERSION,
            requested_role: fluxdown_protocol::ServiceRole::Agent,
            capabilities: Vec::new(),
        };
        let request = fluxdown_protocol::RpcRequest::new(
            fluxdown_protocol::RequestId::Integer(1),
            fluxdown_protocol::method::SYSTEM_HELLO,
            Some(serde_json::to_value(hello).map_err(io::Error::other)?),
        );
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&request)
                    .map_err(io::Error::other)?
                    .into(),
            ))
            .await
            .map_err(io::Error::other)?;
        let message = socket
            .next()
            .await
            .ok_or_else(|| io::Error::other("gateway closed before hello"))?
            .map_err(io::Error::other)?;
        let response: fluxdown_protocol::RpcResponse =
            serde_json::from_str(message.to_text().map_err(io::Error::other)?)
                .map_err(io::Error::other)?;
        match response {
            fluxdown_protocol::RpcResponse::Success(response)
                if response.id == fluxdown_protocol::RequestId::Integer(1) =>
            {
                let hello: fluxdown_protocol::ServiceHello =
                    serde_json::from_value(response.result).map_err(io::Error::other)?;
                if hello.role != fluxdown_protocol::ServiceRole::Agent
                    || hello.protocol_version != fluxdown_protocol::PROTOCOL_VERSION
                {
                    return Err(io::Error::other(
                        "replacement listener returned an incompatible hello",
                    ));
                }
            }
            _ => {
                return Err(io::Error::other(
                    "replacement listener rejected official hello",
                ));
            }
        }
        socket.close(None).await.map_err(io::Error::other)
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "gateway authenticated readiness timed out",
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 双栈监听上的 IPv4 客户端必须以 IPv4 形式出现在 `ConnectInfo` 里，否则回环判定、
    /// 按来源限流与反代信任名单都会把 `::ffff:127.0.0.1` 当成陌生 IPv6 地址。
    #[tokio::test]
    async fn dual_stack_listener_reports_ipv4_peers_canonically() {
        use axum::extract::ConnectInfo;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = crate::runtime::bind_gateway_socket("[::]:0".parse().expect("wildcard"))
            .expect("dual-stack bind");
        let port = listener.local_addr().expect("bound address").port();
        let app = Router::new().route(
            "/peer",
            axum::routing::get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
                peer.ip().to_string()
            }),
        );
        let mut tasks = JoinSet::new();
        let mut running = start_listener(listener, &app, &mut tasks).expect("serve");
        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("IPv4 connect");
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
        running.stop().await;
    }
}
