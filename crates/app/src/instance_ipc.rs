//! 单实例激活通道：次实例把链接 / 激活请求交给主实例后退出。
//!
//! Unix 用 `<instance_dir>/activate.sock`，Windows 用命名管道
//! `\\.\pipe\fluxdown-desktop-<instance_dir FNV hash>`。消息为一行 JSON，主实例
//! 仅在已接收消息后确认；绑定端点先于 UI / agent 初始化，因此并发启动不会丢失唤起。

use std::{
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

const ACKNOWLEDGEMENT: &[u8] = b"ok\n";
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// 激活端点连续故障的首次退避；每次翻倍，封顶 [`LISTENER_RETRY_MAX`]。
const LISTENER_RETRY_BASE: std::time::Duration = std::time::Duration::from_millis(50);
const LISTENER_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(5);

/// 第 `failures` 次（从 1 起）连续故障后的等待：accept / connect / 建实例失败时如果立即重试，
/// 持续性错误（句柄耗尽、实例损坏）会让监听任务空转占满一个核。
fn listener_retry_delay(failures: u32) -> std::time::Duration {
    let doublings = failures.saturating_sub(1).min(16);
    LISTENER_RETRY_BASE
        .saturating_mul(1_u32 << doublings)
        .min(LISTENER_RETRY_MAX)
}

/// 次实例 → 主实例的一次请求。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivateMessage {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub files: Vec<PathBuf>,
    #[serde(default)]
    pub activate: bool,
    #[serde(default)]
    pub settings: bool,
    /// 打开新建下载窗口（`--new-download`）。
    #[serde(default)]
    pub new_download: bool,
    /// agent 为静默建成的任务拉起界面时携带（`--progress-task`）：主实例按用户开始处理该任务。
    #[serde(default)]
    pub progress_task: Option<String>,
}

/// 已排入 UI 事件泵的激活请求；仅在 UI 实际处理后确认给次实例。
pub(crate) struct ActivationRequest {
    message: ActivateMessage,
    acknowledgement: oneshot::Sender<()>,
}

impl ActivationRequest {
    fn new(message: ActivateMessage, acknowledgement: oneshot::Sender<()>) -> Self {
        Self {
            message,
            acknowledgement,
        }
    }

    pub(crate) fn into_parts(self) -> (ActivateMessage, oneshot::Sender<()>) {
        (self.message, self.acknowledgement)
    }
}
/// 通道端点（socket 路径或管道名）。
#[derive(Debug, Clone)]
pub struct Endpoint(String);

impl Endpoint {
    #[must_use]
    pub fn for_instance_dir(instance_dir: &Path) -> Self {
        #[cfg(unix)]
        {
            Self(instance_dir.join("activate.sock").display().to_string())
        }
        #[cfg(windows)]
        {
            let hash = instance_hash(instance_dir);
            Self(format!(r"\\.\pipe\fluxdown-desktop-{hash:016x}"))
        }
    }
}

#[cfg(windows)]
fn instance_hash(instance_dir: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in instance_dir.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// 不能把请求交给主实例的原因。
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("failed to encode FluxDown desktop activation request")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to reach the FluxDown desktop activation endpoint before sending")]
    Io(#[source] io::Error),
    #[error("FluxDown desktop activation may already have been delivered")]
    DeliveryUncertain(#[source] io::Error),
}

impl SendError {
    /// 仅当尚未写出请求时才允许重试，以免捕获请求被重复提交。
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Io(error)
                if is_pipe_busy(error)
                    || matches!(
                        error.kind(),
                        io::ErrorKind::NotFound
                            | io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::NotConnected
                            | io::ErrorKind::BrokenPipe
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::UnexpectedEof
                    )
        )
    }
}

/// 管道实例全被占用（ERROR_PIPE_BUSY）：std 不给它专属 kind，且连接尚未建立，重试是安全的。
fn is_pipe_busy(error: &io::Error) -> bool {
    const ERROR_PIPE_BUSY: i32 = 231;
    cfg!(windows) && error.raw_os_error() == Some(ERROR_PIPE_BUSY)
}

/// 已绑定的主实例激活端点。端点在完整 UI 初始化前建立，以便内核排队并发启动的请求。
pub struct Listener {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(unix)]
    listener: std::os::unix::net::UnixListener,
    #[cfg(windows)]
    name: String,
    #[cfg(windows)]
    server: tokio::net::windows::named_pipe::NamedPipeServer,
}

impl Listener {
    /// 在成为主实例后立即绑定端点。旧 Unix socket 只能来自已经退出的主实例。
    pub fn bind(endpoint: Endpoint) -> Result<Self, io::Error> {
        #[cfg(unix)]
        {
            let path = PathBuf::from(&endpoint.0);
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(error);
            }
            let listener = std::os::unix::net::UnixListener::bind(&path)?;
            listener.set_nonblocking(true)?;
            Ok(Self { path, listener })
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;

            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&endpoint.0)?;
            Ok(Self {
                name: endpoint.0,
                server,
            })
        }
    }

    /// 主实例：将已确认的请求送入 UI 事件泵。
    pub async fn listen(mut self, tx: mpsc::Sender<ActivationRequest>) {
        #[cfg(unix)]
        self.listen_unix(tx).await;
        #[cfg(windows)]
        self.listen_windows(tx).await;
    }

    #[cfg(unix)]
    async fn listen_unix(&mut self, tx: mpsc::Sender<ActivationRequest>) {
        let Ok(listener) = self.listener.try_clone() else {
            log::error!("desktop activation socket unavailable: clone failed");
            return;
        };
        let listener = match tokio::net::UnixListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                log::error!("desktop activation socket unavailable: {error}");
                return;
            }
        };
        let mut failures = 0_u32;
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    failures = failures.saturating_add(1);
                    log::warn!("desktop activation accept failed: {error}");
                    tokio::time::sleep(listener_retry_delay(failures)).await;
                    continue;
                }
            };
            failures = 0;
            let tx = tx.clone();
            tokio::spawn(async move {
                receive_and_ack_unix(stream, tx).await;
            });
        }
    }

    #[cfg(windows)]
    async fn listen_windows(&mut self, tx: mpsc::Sender<ActivationRequest>) {
        let mut failures = 0_u32;
        loop {
            if let Err(error) = self.server.connect().await {
                failures = failures.saturating_add(1);
                log::warn!("desktop activation pipe connect failed: {error}");
                // 同一实例再 connect 会以同一错误立即返回：换新实例（先建后弃，管道名始终有
                // 实例在，并发启动者不会看到 NotFound），并按连续失败次数退避。
                self.server = create_pipe_instance(&self.name).await;
                tokio::time::sleep(listener_retry_delay(failures)).await;
                continue;
            }
            failures = 0;
            let next = create_pipe_instance(&self.name).await;
            let connected = std::mem::replace(&mut self.server, next);
            let tx = tx.clone();
            tokio::spawn(async move {
                receive_and_ack_windows(connected, tx).await;
            });
        }
    }
}

/// 创建下一个管道实例；失败时退避重试而不是放弃，否则主实例此后再也收不到激活请求，
/// 次实例只能等到超时报错。
#[cfg(windows)]
async fn create_pipe_instance(name: &str) -> tokio::net::windows::named_pipe::NamedPipeServer {
    use tokio::net::windows::named_pipe::ServerOptions;

    let mut failures = 0_u32;
    loop {
        match ServerOptions::new().create(name) {
            Ok(server) => return server,
            Err(error) => {
                failures = failures.saturating_add(1);
                log::warn!("desktop activation pipe instance could not be created: {error}");
                tokio::time::sleep(listener_retry_delay(failures)).await;
            }
        }
    }
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            // 监听 socket 可能已被外部移除；其余清理失败保留诊断。
            log::warn!("could not remove desktop activation socket: {error}");
        }
    }
}

/// 次实例：确认主实例已处理请求后才返回成功。
pub fn send_to_primary(endpoint: &Endpoint, message: &ActivateMessage) -> Result<(), SendError> {
    let mut line = serde_json::to_string(message).map_err(SendError::Serialize)?;
    line.push('\n');
    send_line(endpoint, &line)
}

#[cfg(unix)]
fn send_line(endpoint: &Endpoint, line: &str) -> Result<(), SendError> {
    use std::io::{Read as _, Write as _};

    let mut stream = std::os::unix::net::UnixStream::connect(&endpoint.0).map_err(SendError::Io)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(SendError::Io)?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(SendError::Io)?;
    stream
        .write_all(line.as_bytes())
        .map_err(SendError::DeliveryUncertain)?;
    stream.flush().map_err(SendError::DeliveryUncertain)?;
    let mut acknowledgement = [0; ACKNOWLEDGEMENT.len()];
    stream
        .read_exact(&mut acknowledgement)
        .map_err(SendError::DeliveryUncertain)?;
    if acknowledgement == ACKNOWLEDGEMENT {
        Ok(())
    } else {
        Err(SendError::DeliveryUncertain(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected activation acknowledgement",
        )))
    }
}

#[cfg(windows)]
fn send_line(endpoint: &Endpoint, line: &str) -> Result<(), SendError> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::windows::named_pipe::ClientOptions;

    let endpoint = endpoint.0.clone();
    let line = line.to_owned();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(SendError::Io)?;
    runtime.block_on(async move {
        let mut client = ClientOptions::new()
            .open(&endpoint)
            .map_err(SendError::Io)?;
        tokio::time::timeout(IO_TIMEOUT, async move {
            client
                .write_all(line.as_bytes())
                .await
                .map_err(SendError::DeliveryUncertain)?;
            client.flush().await.map_err(SendError::DeliveryUncertain)?;
            let mut acknowledgement = [0; ACKNOWLEDGEMENT.len()];
            client
                .read_exact(&mut acknowledgement)
                .await
                .map_err(SendError::DeliveryUncertain)?;
            if acknowledgement == ACKNOWLEDGEMENT {
                Ok(())
            } else {
                Err(SendError::DeliveryUncertain(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected activation acknowledgement",
                )))
            }
        })
        .await
        .map_err(|_| {
            SendError::DeliveryUncertain(io::Error::new(
                io::ErrorKind::TimedOut,
                "activation acknowledgement timed out",
            ))
        })?
    })
}

#[cfg(unix)]
async fn receive_and_ack_unix(
    mut stream: tokio::net::UnixStream,
    tx: mpsc::Sender<ActivationRequest>,
) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    let mut line = String::new();
    let read = {
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut line).await
    };
    if matches!(read, Ok(count) if count > 0)
        && let Ok(message) = serde_json::from_str::<ActivateMessage>(line.trim())
    {
        let (acknowledgement, confirmed) = oneshot::channel();
        if tx
            .send(ActivationRequest::new(message, acknowledgement))
            .await
            .is_ok()
            && confirmed.await.is_ok()
        {
            if let Err(error) = stream.write_all(ACKNOWLEDGEMENT).await {
                log::debug!("activation peer disconnected before acknowledgement: {error}");
                return;
            }
            if let Err(error) = stream.flush().await {
                log::debug!("activation acknowledgement flush failed: {error}");
            }
        }
    }
}

#[cfg(windows)]
async fn receive_and_ack_windows(
    mut stream: tokio::net::windows::named_pipe::NamedPipeServer,
    tx: mpsc::Sender<ActivationRequest>,
) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    let mut line = String::new();
    let read = {
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut line).await
    };
    if matches!(read, Ok(count) if count > 0)
        && let Ok(message) = serde_json::from_str::<ActivateMessage>(line.trim())
    {
        let (acknowledgement, confirmed) = oneshot::channel();
        if tx
            .send(ActivationRequest::new(message, acknowledgement))
            .await
            .is_ok()
            && confirmed.await.is_ok()
        {
            if let Err(error) = stream.write_all(ACKNOWLEDGEMENT).await {
                log::debug!("activation peer disconnected before acknowledgement: {error}");
                return;
            }
            if let Err(error) = stream.flush().await {
                log::debug!("activation acknowledgement flush failed: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{ActivateMessage, LISTENER_RETRY_MAX, SendError, listener_retry_delay};
    #[cfg(unix)]
    use super::{Endpoint, Listener, send_to_primary};

    #[test]
    fn listener_backoff_doubles_and_is_capped() {
        assert_eq!(listener_retry_delay(1), Duration::from_millis(50));
        assert_eq!(listener_retry_delay(2), Duration::from_millis(100));
        assert_eq!(listener_retry_delay(7), Duration::from_millis(3_200));
        assert_eq!(listener_retry_delay(8), LISTENER_RETRY_MAX);
        assert_eq!(listener_retry_delay(u32::MAX), LISTENER_RETRY_MAX);
    }

    #[cfg(unix)]
    fn test_dir(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::path::PathBuf::from("/tmp").join(format!("fd-{label}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn message_round_trips_and_tolerates_missing_fields() {
        let message = ActivateMessage {
            urls: vec!["https://a/b".to_owned()],
            files: vec!["/tmp/x.torrent".into()],
            activate: true,
            settings: true,
            new_download: true,
            progress_task: Some("task-1".to_owned()),
        };
        let json = serde_json::to_string(&message).expect("serialize");
        assert_eq!(
            serde_json::from_str::<ActivateMessage>(&json).expect("parse"),
            message
        );
        assert_eq!(
            serde_json::from_str::<ActivateMessage>("{}").expect("parse empty"),
            ActivateMessage::default()
        );
    }

    #[cfg(unix)]
    #[test]
    fn queued_request_is_confirmed_after_listener_task_starts() {
        let dir = test_dir("queued");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let endpoint = Endpoint::for_instance_dir(&dir);
        let listener = Listener::bind(endpoint.clone()).expect("bind listener");
        let sent = ActivateMessage {
            urls: vec!["magnet:?xt=abc".to_owned()],
            files: Vec::new(),
            activate: true,
            settings: true,
            new_download: false,
            progress_task: None,
        };
        let sender_endpoint = endpoint.clone();
        let sender_message = sent.clone();
        let sender = std::thread::spawn(move || send_to_primary(&sender_endpoint, &sender_message));

        std::thread::sleep(Duration::from_millis(25));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let task = runtime.spawn(listener.listen(tx));
        let received = runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), rx.recv()).await })
            .expect("timeout")
            .expect("message");
        let (received, acknowledgement) = received.into_parts();
        assert_eq!(received, sent);
        // Cold UI/font initialization can take longer than a single frame.
        std::thread::sleep(Duration::from_millis(400));
        acknowledgement.send(()).expect("confirm activation");
        sender
            .join()
            .expect("sender thread")
            .expect("acknowledged activation");
        task.abort();
        let error = runtime.block_on(task).expect_err("aborted listener task");
        assert!(error.is_cancelled(), "listener task panicked: {error}");
        drop(runtime);
        if let Err(error) = std::fs::remove_dir_all(dir) {
            log::warn!("could not remove queued activation fixture: {error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn missing_listener_is_retryable() {
        let dir = test_dir("missing");
        let endpoint = Endpoint::for_instance_dir(&dir);
        let error =
            send_to_primary(&endpoint, &ActivateMessage::default()).expect_err("no listener");
        assert!(matches!(error, SendError::Io(_)));
        assert!(error.is_retryable());
    }

    #[cfg(windows)]
    #[test]
    fn pipe_busy_is_retryable() {
        let error = SendError::Io(std::io::Error::from_raw_os_error(231));
        assert!(error.is_retryable());
    }

    #[cfg(unix)]
    #[test]
    fn unacknowledged_delivery_is_not_retried() {
        use std::io::Read as _;

        let dir = test_dir("no-ack");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let endpoint = Endpoint::for_instance_dir(&dir);
        let listener = std::os::unix::net::UnixListener::bind(&endpoint.0).expect("bind listener");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            // 只需确认请求已经写达，不读取固定大块而让短请求互相等待。
            let mut request = [0_u8; 1];
            stream
                .read_exact(&mut request)
                .expect("read activation request");
            std::thread::sleep(Duration::from_millis(300));
        });
        let error = send_to_primary(&endpoint, &ActivateMessage::default())
            .expect_err("missing acknowledgement");
        assert!(matches!(error, SendError::DeliveryUncertain(_)));
        assert!(!error.is_retryable());
        server.join().expect("server thread");
        if let Err(error) = std::fs::remove_dir_all(dir) {
            log::warn!("could not remove unacknowledged activation fixture: {error}");
        }
    }
}
