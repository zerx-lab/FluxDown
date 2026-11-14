//! BT 任务级操作的端到端夹具：真实 librqbit 做种会话 + 本地 HTTP tracker + 一台完整引擎。
//!
//! 下载端就是生产同款的 `Engine`（`DownloadManager` + 共享 BT 会话），经 tracker 找到做种端；
//! 做种端可限速，让下载慢到测试能在途中动手，再放开让它跑完。全部流量只走回环地址。

use std::net::{Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fluxdown_engine::bt_config::BtConfig;
use fluxdown_engine::bt_task_files::{BtTaskFiles, BtTaskOpError};
use fluxdown_engine::download_manager::{BtMoveStart, NewTaskSpec, TaskDone};
use fluxdown_engine::model::TaskInfo;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};
use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerOptions, MseMode, Session,
    SessionOptions, TorrentVersion, create_torrent,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;

pub const PIECE_LEN: u32 = 16 * 1024;

/// 种子（容器目录）名。
pub const TORRENT_NAME: &str = "pack";

pub fn unique_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "fluxdown_btops_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// 可复现的文件内容；`seed` 让不同文件的内容不同。
pub fn patterned(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31 + i / 251 + usize::from(seed) * 7) % 251).unwrap_or(0))
        .collect()
}

fn remove_dir(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("test directory {} not removed: {error}", dir.display());
    }
}

/// 本地 HTTP tracker：对任何 announce 都回当前登记的对端（紧凑格式），并数请求数。
pub struct Tracker {
    pub url: String,
    pub addr: SocketAddr,
    peer: Arc<Mutex<Option<SocketAddr>>>,
    hits: Arc<AtomicUsize>,
}

impl Tracker {
    pub async fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("tracker listener");
        let addr = listener.local_addr().expect("tracker addr");
        let peer: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
        let hits = Arc::new(AtomicUsize::new(0));
        let (shared, counter) = (Arc::clone(&peer), Arc::clone(&hits));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let peer = *shared.lock().expect("tracker peer");
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    if socket.read(&mut buf).await.is_err() {
                        return;
                    }
                    let mut body = b"d8:intervali1800e5:peers".to_vec();
                    match peer {
                        Some(SocketAddr::V4(addr)) => {
                            body.extend_from_slice(b"6:");
                            body.extend_from_slice(&addr.ip().octets());
                            body.extend_from_slice(&addr.port().to_be_bytes());
                        }
                        _ => body.extend_from_slice(b"0:"),
                    }
                    body.push(b'e');
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if socket.write_all(head.as_bytes()).await.is_ok()
                        && socket.write_all(&body).await.is_err()
                    {
                        eprintln!("test tracker client went away");
                    }
                });
            }
        });
        Self {
            url: format!("http://127.0.0.1:{}/announce", addr.port()),
            addr,
            peer,
            hits,
        }
    }

    fn set_peer(&self, addr: SocketAddr) {
        *self.peer.lock().expect("tracker peer") = Some(addr);
    }

    /// 收到的连接数（每次 announce 一条连接）。
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// 最小的 SOCKS5 代理（无认证，只做 CONNECT），记下每条隧道的目标地址。
pub struct Socks5 {
    pub addr: SocketAddr,
    targets: Arc<Mutex<Vec<SocketAddr>>>,
}

impl Socks5 {
    pub async fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("proxy listener");
        let addr = listener.local_addr().expect("proxy addr");
        let targets: Arc<Mutex<Vec<SocketAddr>>> = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&targets);
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let targets = Arc::clone(&shared);
                tokio::spawn(async move {
                    // 客户端中途断开等都只是这条隧道结束。
                    if let Err(error) = Self::serve(socket, targets).await {
                        eprintln!("test proxy tunnel ended: {error}");
                    }
                });
            }
        });
        Self { addr, targets }
    }

    async fn serve(
        mut client: tokio::net::TcpStream,
        targets: Arc<Mutex<Vec<SocketAddr>>>,
    ) -> std::io::Result<()> {
        let unsupported = |what: &str| std::io::Error::other(format!("unsupported {what}"));
        // 问候：VER NMETHODS METHODS… → 选「无认证」。
        let mut head = [0u8; 2];
        client.read_exact(&mut head).await?;
        let mut methods = vec![0u8; usize::from(head[1])];
        client.read_exact(&mut methods).await?;
        client.write_all(&[5, 0]).await?;
        // 请求：VER CMD RSV ATYP DST.ADDR DST.PORT。
        let mut request = [0u8; 4];
        client.read_exact(&mut request).await?;
        let host = match request[3] {
            1 => {
                let mut ip = [0u8; 4];
                client.read_exact(&mut ip).await?;
                std::net::IpAddr::from(ip).to_string()
            }
            3 => {
                let mut len = [0u8; 1];
                client.read_exact(&mut len).await?;
                let mut name = vec![0u8; usize::from(len[0])];
                client.read_exact(&mut name).await?;
                String::from_utf8_lossy(&name).into_owned()
            }
            _ => return Err(unsupported("address type")),
        };
        let mut port = [0u8; 2];
        client.read_exact(&mut port).await?;
        if request[1] != 1 {
            // 只做 CONNECT；UDP ASSOCIATE 等一律答「命令不支持」。
            client.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            return Err(unsupported("command"));
        }
        let mut upstream =
            tokio::net::TcpStream::connect((host.as_str(), u16::from_be_bytes(port))).await?;
        targets
            .lock()
            .expect("proxy targets")
            .push(upstream.peer_addr()?);
        client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
        Ok(())
    }

    /// 经代理建立过的到 `target` 的隧道数。
    pub fn tunnels_to(&self, target: SocketAddr) -> usize {
        self.targets
            .lock()
            .expect("proxy targets")
            .iter()
            .filter(|addr| **addr == target)
            .count()
    }
}

/// 做种端：多文件种子 `pack/`，内容在自己的临时目录里。
pub struct Seeder {
    pub session: Arc<Session>,
    /// 做种端的监听地址（tracker 发给下载端的就是它）。
    pub addr: SocketAddr,
    pub torrent_bytes: Vec<u8>,
    /// `(种子内相对路径, 内容)`，按种子里的文件下标排列（混合种子的 padding 文件为全零）。
    pub files: Vec<(String, Vec<u8>)>,
    dir: PathBuf,
}

impl Drop for Seeder {
    fn drop(&mut self) {
        remove_dir(&self.dir);
    }
}

impl Seeder {
    /// `files`：`(种子内相对路径, 内容)`。`upload_bps`：做种端的总上传限速（`None` = 不限）。
    /// 做种端登记到 `tracker`，下载端 announce 时拿到它的地址。
    pub async fn start(
        files: &[(&str, Vec<u8>)],
        tracker: &Tracker,
        upload_bps: Option<u32>,
    ) -> Self {
        Self::start_versioned(files, tracker, upload_bps, TorrentVersion::V1).await
    }

    /// 同 [`Self::start`]，种子格式为 `version`（v1 / v2 / 混合）。
    pub async fn start_versioned(
        files: &[(&str, Vec<u8>)],
        tracker: &Tracker,
        upload_bps: Option<u32>,
        version: TorrentVersion,
    ) -> Self {
        let dir = unique_dir("seed");
        let content = dir.join(TORRENT_NAME);
        for (rel, bytes) in files {
            let path = content.join(rel);
            std::fs::create_dir_all(path.parent().expect("file parent")).expect("content dir");
            std::fs::write(&path, bytes).expect("seed file");
        }
        let torrent = create_torrent(
            &content,
            CreateTorrentOptions {
                name: Some(TORRENT_NAME),
                trackers: vec![tracker.url.clone()],
                piece_length: Some(PIECE_LEN),
                version,
                ..Default::default()
            },
            &librqbit::spawn_utils::BlockingSpawner::new(1),
        )
        .await
        .expect("create torrent");
        let torrent_bytes = torrent.as_bytes().expect("torrent bytes").to_vec();
        // 文件下标取决于种子里的顺序（按路径排序，混合种子另有 padding 文件）：以种子为准重排。
        let layout = fluxdown_engine::bt_task_files::layout_from_torrent_bytes(&torrent_bytes)
            .expect("torrent layout");
        let ordered = layout
            .files
            .iter()
            .map(|fact| {
                if fact.padding {
                    let size = usize::try_from(fact.size).expect("padding size");
                    return (fact.path.clone(), vec![0; size]);
                }
                let (_, bytes) = files
                    .iter()
                    .find(|(rel, _)| *rel == fact.path)
                    .expect("torrent file is one of the inputs");
                (fact.path.clone(), bytes.clone())
            })
            .collect();

        let session = Session::new_with_opts(
            dir.clone(),
            SessionOptions {
                dht: None,
                persistence: None,
                listen: Some(ListenerOptions {
                    listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                    ipv4_only: true,
                    ..Default::default()
                }),
                ipv4_only: true,
                disable_local_service_discovery: true,
                // 做种端自己不找 tracker：tracker 只把它的地址发给下载端。
                disable_trackers: true,
                mse_mode: MseMode::Disabled,
                ratelimits: LimitsConfig {
                    upload_bps: upload_bps.and_then(NonZeroU32::new),
                    download_bps: None,
                },
                ..Default::default()
            },
        )
        .await
        .expect("seed session");
        let handle = session
            .add_torrent(
                AddTorrent::from_bytes(torrent_bytes.clone()),
                Some(AddTorrentOptions {
                    paused: false,
                    output_folder: Some(content.to_string_lossy().into_owned()),
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await
            .expect("add torrent")
            .into_handle()
            .expect("torrent handle");
        timeout(Duration::from_secs(30), handle.wait_until_completed())
            .await
            .expect("seeder did not finish its check in time")
            .expect("seeder check failed");
        let addr = session.listen_addr().expect("listen addr");
        tracker.set_peer(addr);
        Self {
            session,
            addr,
            torrent_bytes,
            files: ordered,
            dir,
        }
    }

    /// 种子内相对路径对应的文件下标。
    pub fn index_of(&self, rel: &str) -> u32 {
        let slot = self
            .files
            .iter()
            .position(|(path, _)| path == rel)
            .unwrap_or_else(|| panic!("{rel} is not in the torrent"));
        u32::try_from(slot).expect("file index")
    }

    pub fn content_of(&self, rel: &str) -> &[u8] {
        let (_, bytes) = self
            .files
            .iter()
            .find(|(path, _)| path == rel)
            .unwrap_or_else(|| panic!("{rel} is not in the torrent"));
        bytes
    }

    /// 放开上传限速，让剩下的下载尽快跑完。
    pub fn unthrottle(&self) {
        self.session.ratelimits.set_upload_bps(None);
    }
}

/// 下载端：一台完整引擎，工作目录用完即删。
pub struct Downloader {
    pub engine: Engine,
    pub work: PathBuf,
    done_rx: mpsc::Receiver<TaskDone>,
}

impl Downloader {
    pub async fn start(tracker: &Tracker) -> Self {
        Self::start_with(tracker, |_, _| {}).await
    }

    /// 同 [`Self::start`]，`configure` 可在起引擎前改 BT 配置与全局代理配置。
    pub async fn start_with(
        tracker: &Tracker,
        configure: impl FnOnce(&mut BtConfig, &mut ProxyConfig),
    ) -> Self {
        let work = unique_dir("leech");
        std::fs::create_dir_all(&work).expect("work dir");
        Self::start_in(work, tracker, configure).await
    }

    /// 关停引擎后在同一个数据目录上重新起一台（模拟应用重启）：任务都不在新会话里。
    pub async fn restart(mut self, tracker: &Tracker) -> Self {
        self.engine.manager.shutdown().await;
        let work = self.work.clone();
        drop(self);
        Self::start_in(work, tracker, |_, _| {}).await
    }

    /// BT 只开 TCP 监听：不碰 DHT / 端口映射 / 局域网发现，追加 tracker 只有测试 tracker
    /// （否则会去连内置的公共 tracker）。
    async fn start_in(
        work: PathBuf,
        tracker: &Tracker,
        configure: impl FnOnce(&mut BtConfig, &mut ProxyConfig),
    ) -> Self {
        let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("ephemeral port")
            .local_addr()
            .expect("local addr")
            .port();
        let mut bt_config = BtConfig {
            enabled: true,
            enable_dht: false,
            enable_upnp: false,
            enable_utp: false,
            enable_lsd: false,
            port_start: port,
            port_end: port,
            custom_trackers: tracker.url.clone(),
            ..Default::default()
        };
        let mut proxy_config = ProxyConfig::default();
        configure(&mut bt_config, &mut proxy_config);
        let mut engine = Engine::new(
            EngineConfig {
                max_concurrent: 2,
                speed_limit_bps: 0,
                upload_limit_bps: 0,
                default_save_dir: work.to_string_lossy().into_owned(),
                app_data_dir: work.to_string_lossy().into_owned(),
                bt_config,
                proxy_config,
                user_agent: String::new(),
                data_dir_override: Some(work.clone()),
                database_url: None,
            },
            Arc::new(NoopSink),
            Arc::new(NoopSelection),
        )
        .await
        .expect("engine");
        let done_rx = engine.manager.take_done_rx().expect("done receiver");
        let mut progress_rx = engine
            .manager
            .take_progress_rx()
            .expect("progress receiver");
        tokio::spawn(async move { while progress_rx.recv().await.is_some() {} });
        Self {
            engine,
            work,
            done_rx,
        }
    }

    pub async fn add(&mut self, seeder: &Seeder, selected: &[u32], start_paused: bool) -> String {
        self.add_with_route(seeder, selected, start_paused, None)
            .await
    }

    /// 用种子字节建任务，并可指定分类目录路由。
    pub async fn add_with_route(
        &mut self,
        seeder: &Seeder,
        selected: &[u32],
        start_paused: bool,
        category_route: Option<fluxdown_engine::category_route::CategoryRoute>,
    ) -> String {
        self.engine
            .manager
            .create_task(NewTaskSpec {
                save_dir: self.work.to_string_lossy().into_owned(),
                torrent_file_bytes: seeder.torrent_bytes.clone(),
                selected_file_indices: selected
                    .iter()
                    .map(|index| i32::try_from(*index).expect("file index"))
                    .collect(),
                unattended_selection: selected.is_empty(),
                start_paused,
                category_route,
                ..Default::default()
            })
            .await
            .expect("create BT task")
    }

    #[cfg(feature = "plugins")]
    pub async fn install_archive_plugin(&self, script: &str) {
        let dir = self.work.join("archive_plugin");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create archive plugin directory");
        tokio::fs::write(
            dir.join("manifest.json"),
            r#"{"identity":"test@bt-archive","name":"BT archive test","version":"1.0.0","permissions":["archive"],"contributes":{"archive":"archive.js"}}"#,
        )
        .await
        .expect("write archive manifest");
        tokio::fs::write(dir.join("archive.js"), script)
            .await
            .expect("write archive script");
        let plugins = self
            .engine
            .manager
            .plugin_manager()
            .expect("plugin manager");
        let id = plugins
            .install_dev(&dir)
            .await
            .expect("install archive plugin");
        plugins
            .grant_consent(&id, &["archive".to_owned()], &[])
            .await
            .expect("grant archive permission");
        plugins
            .set_enabled(&id, true)
            .await
            .expect("enable archive plugin");
    }

    #[cfg(feature = "plugins")]
    /// After the ordinary completion has been acknowledged, drain manager work until the
    /// task's save directory reaches the requested archive destination.
    pub async fn drain_done_until_save_dir(&mut self, task_id: &str, save_dir: &Path) -> TaskInfo {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let task = self.task(task_id).await;
            if Path::new(&task.save_dir) == save_dir {
                return task;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for BT archive destination"
            );
            let done = timeout(remaining, self.done_rx.recv())
                .await
                .expect("BT archive work did not finish in time")
                .expect("done channel open");
            self.engine.manager.on_task_done(&done).await;
        }
    }

    /// 用磁力链接建任务（全部文件，无人值守）：元数据经 BEP 9 从做种端取。
    pub async fn add_magnet(&mut self, magnet: &str) -> String {
        self.engine
            .manager
            .create_task(NewTaskSpec {
                url: magnet.to_owned(),
                save_dir: self.work.to_string_lossy().into_owned(),
                unattended_selection: true,
                ..Default::default()
            })
            .await
            .expect("create magnet task")
    }

    pub async fn task(&self, task_id: &str) -> TaskInfo {
        self.engine
            .db
            .load_task_by_id(task_id)
            .await
            .expect("load task")
            .expect("task exists")
    }

    pub async fn files(&self, task_id: &str) -> BtTaskFiles {
        self.engine
            .manager
            .bt_task_files(task_id)
            .await
            .expect("bt task files")
    }

    /// 轮询文件表直到 `cond` 成立，返回那一刻的文件表。
    pub async fn wait_for_files(
        &self,
        task_id: &str,
        what: &str,
        mut cond: impl FnMut(&BtTaskFiles) -> bool,
    ) -> BtTaskFiles {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let files = self.files(task_id).await;
            if cond(&files) {
                return files;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; files: {files:?}; task error: {:?}",
                self.task(task_id).await.error_message
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 等任务的下载过程结束并交回 manager 收尾，返回落库的任务行。
    pub async fn wait_done(&mut self, task_id: &str) -> TaskInfo {
        loop {
            let done = timeout(Duration::from_secs(120), self.done_rx.recv())
                .await
                .expect("download did not end in time")
                .expect("done channel open");
            let finished = done.task_id == task_id;
            self.engine.manager.on_task_done(&done).await;
            if finished {
                return self.task(task_id).await;
            }
        }
    }

    /// 移动任务数据，走与 daemon actor 相同的流程：下载中的任务先暂停，等它的 TaskDone 交回
    /// manager 再继续；搬移作业在这里 await，结果交回 manager 落库并恢复任务状态。
    pub async fn move_storage(
        &mut self,
        task_id: &str,
        save_dir: &Path,
    ) -> Result<(), BtTaskOpError> {
        let save_dir = save_dir.to_string_lossy().into_owned();
        let mut resume_after = false;
        loop {
            match self
                .engine
                .manager
                .begin_bt_move(task_id, &save_dir, resume_after)
                .await?
            {
                BtMoveStart::Unchanged => return Ok(()),
                BtMoveStart::Wait {
                    resume_after: again,
                } => {
                    resume_after = again;
                    let done = timeout(Duration::from_secs(60), self.done_rx.recv())
                        .await
                        .expect("the paused download did not stop in time")
                        .expect("done channel open");
                    self.engine.manager.on_task_done(&done).await;
                }
                BtMoveStart::Started(job) => {
                    let outcome = job.await;
                    return self.engine.manager.finish_bt_move(outcome).await;
                }
            }
        }
    }

    /// 任务的 staging 目录（未完成的数据在这里）。
    pub async fn stage_dir(&self, task_id: &str) -> PathBuf {
        let task = self.task(task_id).await;
        Path::new(&task.save_dir).join(format!(".bt_stage_{task_id}"))
    }

    /// 完成后种子内相对路径 `rel` 落盘的内容；没有落盘返回 `None`。
    ///
    /// 完成布局有两种：选中的文件带子目录时建容器目录（`save_dir/file_name/`）并在其下保持
    /// 相对路径；都在顶层时各文件平铺在保存目录里。
    pub async fn landed(&self, task: &TaskInfo, rel: &str) -> Option<Vec<u8>> {
        let save_dir = Path::new(&task.save_dir);
        let root = save_dir.join(&task.file_name);
        let path = if root.is_dir() {
            root.join(rel)
        } else {
            save_dir.join(Path::new(rel).file_name().expect("file name"))
        };
        match tokio::fs::read(&path).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("reading {}: {error}", path.display()),
        }
    }

    pub async fn shutdown(mut self) {
        self.engine.manager.shutdown().await;
        let work = self.work.clone();
        drop(self);
        remove_dir(&work);
    }
}
