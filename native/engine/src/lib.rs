//! `fluxdown_engine` —— FluxDown 下载引擎,零 FFI 依赖。
//!
//! 本 crate 承载 HTTP/FTP/BT/HLS/DASH 下载核心逻辑,通过 [`events::EventSink`]
//! 与 [`selection::HostSelection`] 两个 trait 与宿主(hub/CLI/Web+Server/Phone)
//! 解耦,不绑定具体的 FFI/信号/传输协议。

pub mod bt_downloader;
pub mod components;
pub mod dash_downloader;
pub mod data_dir;
pub mod db;
pub mod disk_space;
pub mod download_manager;
pub mod downloader;
pub mod ed2k;
pub mod events;
pub mod ftp_downloader;
pub mod hls_downloader;
pub mod logger;
pub mod meta_prober;
pub mod model;
/// 插件系统（可选、可失败的下载中间层）。仅 `plugins` feature 下编译。
#[cfg(feature = "plugins")]
pub mod plugin;
pub mod proxy_config;
pub mod segment_advisor;
pub mod segment_coordinator;
pub mod selection;
pub mod speed_limiter;
pub mod tracker_subscription;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bt_downloader::BtConfig;
use db::{Db, DbError};
use download_manager::{DownloadManager, DownloadManagerConfig};
use downloader::DownloadError;
use events::{EngineEvent, EventSink};
use model::{BtFileEntry, HlsQualityOption, TorrentMetaResult};
use proxy_config::ProxyConfig;
use selection::{HostSelection, SelectionOutcome};

/// [`Engine::new`] 的配置聚合。字段来源于现有 `DownloadManagerConfig`(平移)
/// + `data_dir_override`(新增,接 [`data_dir::resolve_data_dir`])。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::EngineConfig;
/// use fluxdown_engine::bt_downloader::BtConfig;
/// use fluxdown_engine::proxy_config::ProxyConfig;
///
/// let config = EngineConfig {
///     max_concurrent: 5,
///     speed_limit_bps: 0,
///     default_save_dir: "/tmp/downloads".to_string(),
///     app_data_dir: "/tmp/fluxdown".to_string(),
///     bt_config: BtConfig::default(),
///     proxy_config: ProxyConfig::default(),
///     user_agent: String::new(),
///     data_dir_override: None,
///     database_url: None,
/// };
/// assert_eq!(config.max_concurrent, 5);
/// ```
pub struct EngineConfig {
    pub max_concurrent: usize,
    pub speed_limit_bps: u64,
    pub default_save_dir: String,
    pub app_data_dir: String,
    pub bt_config: BtConfig,
    pub proxy_config: ProxyConfig,
    pub user_agent: String,
    /// 显式指定数据目录(DB/日志等)。`None` 时回退平台自动探测
    /// (portable marker / `LOCALAPPDATA` / XDG / macOS Application Support)。
    pub data_dir_override: Option<PathBuf>,
    /// 数据库连接 URL。`None` = 数据目录下的 SQLite 文件(桌面默认);
    /// `Some("sqlite:…")` / `Some("postgres://…")` = 按 URL 连接
    /// (headless 服务器用,见 [`db::Db::connect`](Db::connect))。
    pub database_url: Option<String>,
}

/// [`Engine::new`] 可能失败的原因。透明转发底层三个子系统的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Download(#[from] DownloadError),
    #[error(transparent)]
    DataDir(#[from] data_dir::DataDirError),
    /// 插件系统初始化失败（仅 `plugins` feature）。
    #[error("插件系统初始化失败: {0}")]
    Plugin(String),
}

/// FluxDown 下载引擎 facade —— 零 FFI 依赖,是本 crate 的唯一公开入口。
///
/// 聚合 [`db::Db`](Db)(持久化)与
/// [`download_manager::DownloadManager`](DownloadManager)(任务生命周期/
/// 并发调度/协议分发)。`db`/`manager` 为公开字段而非私有 + 转发方法
/// ——`DownloadManager` 现有方法数量庞大(创建/暂停/恢复/取消/删除/队列
/// 管理/…),逐一包一层零逻辑转发只会制造样板代码,不增加任何抽象价值。
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};
/// use fluxdown_engine::bt_downloader::BtConfig;
/// use fluxdown_engine::proxy_config::ProxyConfig;
///
/// # async fn run() -> Result<(), fluxdown_engine::EngineError> {
/// let config = EngineConfig {
///     max_concurrent: 5,
///     speed_limit_bps: 0,
///     default_save_dir: "/tmp/downloads".to_string(),
///     app_data_dir: "/tmp/fluxdown".to_string(),
///     bt_config: BtConfig::default(),
///     proxy_config: ProxyConfig::default(),
///     user_agent: String::new(),
///     data_dir_override: None,
///     database_url: None,
/// };
/// let mut engine = Engine::new(config, Arc::new(NoopSink), Arc::new(NoopSelection)).await?;
/// engine.manager.load_and_send_all_tasks().await;
/// # Ok(())
/// # }
/// ```
pub struct Engine {
    /// 数据库句柄。`DownloadManager` 内部持有自己的 clone;此处额外持有
    /// 一份供宿主直接做 config/task 查询(如 `RequestConfig`/`SaveConfig`),
    /// 不必逐一在 `Engine` 上转发 `Db` 的每个方法。
    pub db: Db,
    /// 任务生命周期管理器。
    pub manager: DownloadManager,
    /// 需要宿主介入决策的选择接口(HLS 画质/BT 文件选择),与传入
    /// [`Engine::new`] 的实例相同 —— 供宿主收到"投递答案"信号时直接调用
    /// `engine.selector.provide_*(...)`,不必另行持有一份引用。
    pub selector: Arc<dyn HostSelection>,
    /// 解析后的数据目录（含 override）。供宿主调用 `components::*` API
    /// （ffmpeg 探测/安装）时传入,与引擎内部使用的目录保持一致。
    pub data_dir: PathBuf,
}

impl Engine {
    /// 唯一构造入口:解析数据目录 → 打开数据库 → 构造
    /// [`DownloadManager`]。
    ///
    /// # Errors
    ///
    /// 数据目录解析失败、数据库打开失败或 HTTP client 构建失败(代理配置
    /// 非法)时返回 [`EngineError`]。
    pub async fn new(
        config: EngineConfig,
        sink: Arc<dyn EventSink>,
        selector: Arc<dyn HostSelection>,
    ) -> Result<Self, EngineError> {
        let data_dir = data_dir::resolve_data_dir(config.data_dir_override.as_deref())?;
        let db = match &config.database_url {
            Some(url) => Db::connect(url).await?,
            None => Db::open(&data_dir).await?,
        };
        // 读回持久化的域名连接上限观察（过期/旧版本数据在加载时丢弃）。
        segment_coordinator::load_domain_conn_caps(&db).await;
        // 插件系统构造所需值需在 config 被 move 进 DownloadManagerConfig 前克隆。
        #[cfg(feature = "plugins")]
        let plugin_ctx = (
            config.proxy_config.clone(),
            sink.clone(),
            config.max_concurrent,
            data_dir.clone(),
        );
        #[cfg_attr(not(feature = "plugins"), allow(unused_mut))]
        let mut manager = DownloadManager::new(
            db.clone(),
            DownloadManagerConfig {
                max_concurrent: config.max_concurrent,
                speed_limit_bps: config.speed_limit_bps,
                default_save_dir: config.default_save_dir,
                app_data_dir: config.app_data_dir,
                data_dir: data_dir.clone(),
                bt_config: config.bt_config,
                proxy_config: config.proxy_config,
                user_agent: config.user_agent,
            },
            sink,
            selector.clone(),
        )?;
        // 组装并注入插件管理器（feature 关时整块不编译，下载主链路零变化）。
        #[cfg(feature = "plugins")]
        {
            use std::sync::Arc as StdArc;
            let (proxy_cfg, plugin_sink, max_conc, data_dir_p) = plugin_ctx;
            let retry_tx = manager.plugin_retry_sender();
            let bridge: StdArc<dyn plugin::PluginBridge> = StdArc::new(
                plugin::bridge::EngineBridge::new(
                    db.clone(),
                    &proxy_cfg,
                    retry_tx,
                    data_dir_p.clone(),
                )
                .map_err(|e| EngineError::Plugin(e.to_string()))?,
            );
            let runtime: StdArc<dyn plugin::ScriptRuntime> = StdArc::new(
                plugin::quickjs::QuickJsScriptRuntime::new(max_conc)
                    .map_err(|e| EngineError::Plugin(e.to_string()))?,
            );
            let app_version = db
                .get_config("app_version")
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let plugins_root = data_dir_p.join("plugins");
            let _ = tokio::fs::create_dir_all(&plugins_root).await;
            let pm = StdArc::new(plugin::PluginManager::new(
                runtime,
                bridge,
                db.clone(),
                plugins_root,
                app_version,
                plugin_sink,
            ));
            pm.load_all().await;
            manager.install_plugin_manager(pm);
        }
        Ok(Self {
            db,
            manager,
            selector,
            data_dir,
        })
    }

    /// 测试代理连通性,返回延迟(毫秒)。
    ///
    /// 封装原本被 `download_actor.rs` 绕过 `DownloadManager` 直接调用的
    /// 自由函数 `proxy_config::test_proxy_connection`(逐字复用)。
    pub async fn test_proxy_connection(
        &self,
        proxy_type: &str,
        proxy_host: &str,
        proxy_port: &str,
        proxy_username: &str,
        proxy_password: &str,
    ) -> Result<i64, DownloadError> {
        proxy_config::test_proxy_connection(
            proxy_type,
            proxy_host,
            proxy_port,
            proxy_username,
            proxy_password,
        )
        .await
    }

    /// 解析 `.torrent` 文件内容(不创建下载任务),用于新建下载对话框预览。
    ///
    /// 封装原本被 `download_actor.rs` 绕过 `DownloadManager` 直接调用的
    /// 自由函数 `bt_downloader::probe_torrent_meta`;内部 `spawn_blocking`,
    /// 把"不阻塞 current_thread runtime"这条现由 hub 手动承担的责任收进
    /// Engine 内部。解析本身是纯 CPU 计算(无网络),`spawn_blocking` 的
    /// `JoinError`(仅 panic 才会发生)转换为一条 `TorrentMetaResult.error`,
    /// 而不是让调用方处理一个额外的 `Result` 分支。
    pub async fn probe_torrent_meta(
        &self,
        probe_id: String,
        torrent_bytes: Vec<u8>,
    ) -> TorrentMetaResult {
        let probe_id_for_panic = probe_id.clone();
        tokio::task::spawn_blocking(move || {
            bt_downloader::probe_torrent_meta(probe_id, torrent_bytes)
        })
        .await
        .unwrap_or_else(|e| TorrentMetaResult {
            probe_id: probe_id_for_panic,
            name: String::new(),
            total_bytes: 0,
            files: Vec::new(),
            error: format!("probe task panicked: {e}"),
        })
    }
}

/// 零依赖的默认 [`EventSink`] 实现:`emit` 直接转发到 `tracing::debug!`。
/// 供 headless/CLI/测试场景开箱即用,不必每个调用方都手写一个空实现。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::NoopSink;
/// use fluxdown_engine::events::{EngineEvent, EventSink};
///
/// let sink = NoopSink;
/// sink.emit(EngineEvent::TaskMetaProbed {
///     task_id: "t1".to_string(),
///     file_name: "a.bin".to_string(),
///     total_bytes: 0,
/// });
/// ```
pub struct NoopSink;

impl EventSink for NoopSink {
    fn emit(&self, event: EngineEvent) {
        tracing::debug!("[noop-sink] {event:?}");
    }
}

/// 零依赖的默认 [`HostSelection`] 实现:两个 `select_*` 方法都立即返回
/// `NoSelectorConfigured`,不进入等待。供 headless/测试场景直接使用。
///
/// `select_hls_quality` 返回带宽最高的变体索引(与
/// `hls_downloader`/`dash_downloader` 内部 `auto_select_best` 的既有语义
/// 一致);`select_bt_files` 返回空 vec —— 复用 `bt_downloader.rs` 现状
/// "空 vec = 下载全部文件"的既有语义,不引入新含义。
///
/// `provide_hls_selection`/`provide_bt_selection` 是空操作:没有调用方在
/// 等待,投递答案无事可做。
///
/// # Examples
///
/// ```
/// # async fn run() {
/// use fluxdown_engine::NoopSelection;
/// use fluxdown_engine::selection::{HostSelection, SelectionOutcome};
/// use std::time::Duration;
///
/// let selector = NoopSelection;
/// let outcome = selector.select_bt_files("task1", &[], None).await;
/// assert_eq!(outcome, SelectionOutcome::NoSelectorConfigured(vec![]));
/// # }
/// ```
pub struct NoopSelection;

#[async_trait::async_trait]
impl HostSelection for NoopSelection {
    async fn select_hls_quality(
        &self,
        _task_id: &str,
        options: &[HlsQualityOption],
        _timeout: Duration,
    ) -> SelectionOutcome<i32> {
        let best = options
            .iter()
            .enumerate()
            .max_by_key(|(_, o)| o.bandwidth)
            .map(|(i, _)| i as i32)
            .unwrap_or(0);
        SelectionOutcome::NoSelectorConfigured(best)
    }

    async fn select_bt_files(
        &self,
        _task_id: &str,
        _files: &[BtFileEntry],
        _timeout: Option<Duration>,
    ) -> SelectionOutcome<Vec<i32>> {
        SelectionOutcome::NoSelectorConfigured(Vec::new())
    }
    async fn select_resolve_variant(
        &self,
        _task_id: &str,
        _options: &[crate::model::ResolveVariantOption],
        default_index: i32,
        _timeout: Duration,
    ) -> SelectionOutcome<i32> {
        SelectionOutcome::NoSelectorConfigured(default_index)
    }

    fn provide_hls_selection(&self, _task_id: &str, _selected_index: i32) {}

    fn provide_bt_selection(&self, _task_id: &str, _selected_indices: Vec<i32>) {}
    fn provide_variant_selection(&self, _task_id: &str, _selected_index: i32) {}
}
