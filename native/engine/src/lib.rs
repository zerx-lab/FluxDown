//! `fluxdown_engine` —— FluxDown 下载引擎,零 FFI 依赖。
//!
//! 本 crate 承载 HTTP/FTP/BT/HLS/DASH 下载核心逻辑,通过 [`events::EventSink`]
//! 与 [`selection::HostSelection`] 两个 trait 与宿主（daemon / CLI / 原生移动端）
//! 解耦,不绑定具体的 FFI/信号/传输协议。

/// 插件通用认证凭据存储与请求注入。
pub mod auth;
/// `ProxyMode::Auto` 探测阶段的多路径竞速（Happy Eyeballs 式错峰并行探测）。
pub(crate) mod auto_probe;
/// `ProxyMode::Auto`：候选代理解析、多路径任务上下文与路由 wire 标签。
pub mod auto_proxy;
/// BitTorrent / 磁力配置与链接识别（始终编译；无 `bt` feature 时用于明确拒绝）。
pub mod bt_config;
/// BitTorrent 会话与下载实现（librqbit）。仅 `bt` feature 下编译。
#[cfg(feature = "bt")]
pub mod bt_downloader;
/// BT 防吸血（peer 行为检测 / 封禁账本 / 云名单应用）。与 `bt` feature 同门控。
#[cfg(feature = "bt")]
pub mod bt_guard;
/// 本机文件做种：为磁盘上已有的文件 / 文件夹生成或匹配种子并登记为做种任务。
#[cfg(feature = "bt")]
pub mod bt_local_seed;
/// BT 部分选择做种的 parts 边车（选中文件路径映射 + 跨文件边界字节）。
#[cfg(feature = "bt")]
pub mod bt_partfile;
pub mod bt_seeding;
/// Windows 的 NTFS sparse 标记（HTTP 多段预分配与 BT staging 共用）；BT staging
/// 存储包装仅 `bt` feature 下编译。
#[cfg(target_os = "windows")]
pub mod bt_sparse;
/// BT 任务运行时状态：peer 列表与 tracker 状态（tracker 状态经 tracing 旁路采集）。
#[cfg(feature = "bt")]
pub mod bt_status;
/// 已完成 BT 任务的完整性校验（按 piece 表重哈希落盘数据）。
#[cfg(feature = "bt")]
pub mod bt_verify;
/// 外部捕获的分类保存目录路由：建任务时选初始目录，完成前按最终文件名复核落点。
pub mod category_route;
pub mod cdn;
pub(crate) mod client_cache;
pub mod components;
/// 请求凭据（Cookie / Authorization / Referer）对锚点 URL 的作用域判定（公共后缀表）。
pub(crate) mod credential_scope;
pub mod dash_downloader;
pub mod data_dir;
pub mod db;
pub mod disk_space;
pub mod download_manager;
pub mod downloader;
/// eD2K / Kad 协议栈。仅 `ed2k` feature 下编译。
#[cfg(feature = "ed2k")]
pub mod ed2k;
/// 宿主自身外网请求（应用更新、组件、插件包、订阅列表）的统一出口：全局代理 + 插件改写。
pub mod egress;
pub mod events;
pub mod file_exists;
pub mod ftp_downloader;
/// IP → 国家（peer 国旗）：最小 MaxMind DB 读取器 + DB-IP 国家库按月下载。
#[cfg(feature = "bt")]
pub mod geoip;
pub mod hls_downloader;
/// 下载请求发送：逐跳跟随重定向，每跳按凭据作用域重建请求头。
pub(crate) mod http_request;
pub mod intercept;
pub mod logger;
/// 下载来源标记（Windows Zone.Identifier / macOS quarantine）。
pub mod mark_of_the_web;
pub mod meta_prober;
pub mod model;
/// 多网卡聚合下载：网卡枚举、链路规划与出口绑定。
pub mod multi_nic;
/// 下载文件名命名管线：Content-Disposition 解码、URL / 类型证据、sanitize、魔数嗅探。
pub mod naming;
/// 网络传输失败的类型化分类（换路 / 自动重试的唯一判据，平台与语言无关）。
pub mod net_failure;
pub(crate) mod output;
/// 系统 PAC：脚本获取与缓存、按下载链接决策、失败代理记忆。
pub(crate) mod pac;
/// 系统 PAC 脚本的 `FindProxyForURL` JS 执行器（QuickJS，仅 `plugins` feature）。
#[cfg(feature = "plugins")]
pub(crate) mod pac_js;
/// 任务起跑时按系统 PAC 修正出口（System / Auto 模式）。
pub(crate) mod pac_route;
/// 多路径分段调度的纯判据（稳态速率采样、竞争集、均衡拆分、完成时间抢占）。
pub(crate) mod path_scheduler;
/// 插件系统（可选、可失败的下载中间层）。仅 `plugins` feature 下编译。
#[cfg(feature = "plugins")]
pub mod plugin;
mod proc;
pub mod proxy_config;
/// 站点代理规则：规则模型、严格校验、逐请求匹配与「测试网址」解释。
pub mod proxy_rules;
/// 排队顺序的增量发布（稳定排序键 + 差异计算）。
pub mod queue_order;
/// `ProxyMode::Auto` 各路径单连接速率的跨重启折扣先验。
pub mod route_health;
/// RSS 订阅自动下载（feed 轮询 → 规则过滤 → 建任务）。
pub mod rss;
/// 站点代理规则的逐请求路由（`RequestSpec.egress`）。
pub mod rule_route;
pub mod segment_advisor;
pub mod segment_coordinator;
pub mod selection;
/// 站点 HTTP Basic 认证凭据（per-host 保存 + 建任务时自动套用）。
pub mod site_auth;
/// 任务整个下载期的速度曲线与统计（持久化，任务完成后可回看）。
pub mod speed_history;
pub mod speed_limiter;
/// 通用订阅 provider 接口（RSS 与插件订阅共用）。
pub mod subscription;
pub mod task_activity;
mod temp_file_guard;
/// `thunder://` 链接解析（迅雷专有 base64 封装：`AA<真实地址>ZZ`）。
pub mod thunder;
/// BT tracker 列表订阅。仅 `bt` feature 下编译。
#[cfg(feature = "bt")]
pub mod tracker_subscription;
pub mod transfer_activity;
/// 用户主目录下的系统标准目录（下载目录：Windows 已知文件夹 / XDG user-dirs）。
pub mod user_dirs;

/// 任务事件 Webhook 推送（免费自托管，BYOE）。
pub mod webhook;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bt_config::BtConfig;
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
/// use fluxdown_engine::bt_config::BtConfig;
/// use fluxdown_engine::proxy_config::ProxyConfig;
///
/// let config = EngineConfig {
///     max_concurrent: 5,
///     speed_limit_bps: 0,
///     upload_limit_bps: 0,
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
    /// 全局 BT 上传限速（B/s，0 = 不限）。与 `speed_limit_bps` 解耦：
    /// 后者只管下载，本字段管 BT 上传（下载期上传 + 做种）。
    pub upload_limit_bps: u64,
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
/// use fluxdown_engine::bt_config::BtConfig;
/// use fluxdown_engine::proxy_config::ProxyConfig;
///
/// # async fn run() -> Result<(), fluxdown_engine::EngineError> {
/// let config = EngineConfig {
///     max_concurrent: 5,
///     speed_limit_bps: 0,
///     upload_limit_bps: 0,
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
    /// 与 manager 共用的源端活动代理；宿主的 progress_reporter 也必须用它。
    pub activity_sink: Arc<dyn EventSink>,
    activity_journal: Arc<task_activity::JournalSink>,
    /// 需要宿主介入决策的选择接口(HLS 画质/BT 文件选择),与传入
    /// [`Engine::new`] 的实例相同 —— 供宿主收到"投递答案"信号时直接调用
    /// `engine.selector.provide_*(...)`,不必另行持有一份引用。
    pub selector: Arc<dyn HostSelection>,
    /// 解析后的数据目录（含 override）。供宿主调用 `components::*` API
    /// （ffmpeg 探测/安装）时传入,与引擎内部使用的目录保持一致。
    pub data_dir: PathBuf,
    /// 唯一写入者租约；字段顺序保证在数据库句柄之后释放。宿主经
    /// [`Self::write_guard`] 取得共享句柄做租约心跳。
    write_guard: Arc<db::EngineWriteGuard>,
}

impl Engine {
    /// 解析数据目录、打开数据库并构造 [`DownloadManager`]。
    ///
    /// 已经为了读取启动配置打开数据库的宿主应改用 [`Self::from_db`]，避免
    /// 重复创建连接池并再次执行 schema 初始化。
    ///
    /// # Errors
    ///
    /// 数据目录解析失败、数据库打开失败或 HTTP client 构建失败（代理配置
    /// 非法）时返回 [`EngineError`]。
    pub async fn new(
        config: EngineConfig,
        sink: Arc<dyn EventSink>,
        selector: Arc<dyn HostSelection>,
    ) -> Result<Self, EngineError> {
        let data_dir = data_dir::resolve_data_dir(config.data_dir_override.as_deref())?;
        let (db, write_guard) = match &config.database_url {
            Some(url) => Db::connect_exclusive(url, &data_dir).await?,
            None => Db::open_exclusive(&data_dir).await?,
        };
        Self::initialize(config, data_dir, db, write_guard, sink, selector).await
    }

    /// 使用宿主已打开的数据库连接池构造引擎。
    ///
    /// 该入口与 [`Self::new`] 的运行时行为相同，只跳过数据库打开与 schema
    /// 初始化。调用方必须传入与 `config.database_url`、数据目录相对应的
    /// [`Db`]；引擎仍会执行队列播种、缓存恢复和插件/RSS/Webhook 装载。
    ///
    /// # Errors
    ///
    /// 数据目录解析失败或 HTTP client 构建失败（代理配置非法）时返回
    /// [`EngineError`]。
    pub async fn from_db(
        config: EngineConfig,
        db: Db,
        write_guard: db::EngineWriteGuard,
        sink: Arc<dyn EventSink>,
        selector: Arc<dyn HostSelection>,
    ) -> Result<Self, EngineError> {
        let data_dir = data_dir::resolve_data_dir(config.data_dir_override.as_deref())?;
        Self::initialize(config, data_dir, db, write_guard, sink, selector).await
    }

    async fn initialize(
        config: EngineConfig,
        data_dir: PathBuf,
        db: Db,
        write_guard: db::EngineWriteGuard,
        sink: Arc<dyn EventSink>,
        selector: Arc<dyn HostSelection>,
    ) -> Result<Self, EngineError> {
        // 读回持久化的域名连接上限观察（过期/旧版本数据在加载时丢弃）。
        segment_coordinator::load_domain_conn_caps(&db).await;
        // 读回持久化的 CDN 节点健康度（多节点聚合的跨任务先验，同上范式）。
        cdn::health::load_cdn_health(&db).await;
        // 读回持久化的 Auto 代理路由先验（换网/过期/旧版本在加载时丢弃）。
        route_health::load(&db).await;
        // 读回云端下发的 resolver 端点清单（空/非法 → 内置 baseline）。
        cdn::resolver::load_endpoints_from_config(&db).await;
        // 读回云端下发的 ECS 探测子网与 hints base（P2；空 = 各自禁用）。
        cdn::resolver::load_ecs_from_config(&db).await;
        cdn::hints::load_base_from_config(&db).await;
        // 遥测：回读上次进程遗留的待上传样本（采样常开）。
        cdn::telemetry::load_pending(&db).await;
        // 播种内置队列（main 主队列 / later 稍后下载，幂等）并迁移存量
        // 空 queue_id 任务——必须先于宿主的 `manager.load_queues()`。
        db.seed_builtin_queues().await?;
        // 组 GC：清理无成员的孤儿组行（重启后一次性清理；无事件——首次
        // send_all_groups 由宿主主动调用触发，此处不广播）。
        if let Err(error) = db.gc_empty_groups().await {
            logger::report_warning("engine", "garbage collect empty groups", &error);
        }
        // 插件系统构造所需值需在 config 被 move 进 DownloadManagerConfig 前克隆。
        let activity_journal = task_activity::JournalSink::start(db.clone(), sink);
        let sink: Arc<dyn EventSink> = activity_journal.clone();
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
                upload_limit_bps: config.upload_limit_bps,
                default_save_dir: config.default_save_dir,
                app_data_dir: config.app_data_dir,
                data_dir: data_dir.clone(),
                bt_config: config.bt_config,
                proxy_config: config.proxy_config,
                user_agent: config.user_agent,
            },
            sink.clone(),
            selector.clone(),
        )?;
        // 组装并注入插件管理器（feature 关时整块不编译，下载主链路零变化）。
        #[cfg(feature = "plugins")]
        {
            let (proxy_cfg, plugin_sink, max_conc, data_dir_p) = plugin_ctx;
            let retry_tx = manager.plugin_retry_sender();
            let runtime: Arc<dyn plugin::ScriptRuntime> = Arc::new(
                plugin::quickjs::QuickJsScriptRuntime::new(max_conc)
                    .map_err(|e| EngineError::Plugin(e.to_string()))?,
            );
            let audit = plugin::audit::AuditLog::new(db.clone(), &runtime.spawn_handle());
            let engine_bridge = Arc::new(
                plugin::bridge::EngineBridge::new(
                    db.clone(),
                    &proxy_cfg,
                    retry_tx,
                    data_dir_p.clone(),
                )
                .map_err(|e| EngineError::Plugin(e.to_string()))?,
            );
            let bridge: Arc<dyn plugin::PluginBridge> = Arc::new(
                plugin::audit::AuditingBridge::new(engine_bridge.clone(), audit.clone()),
            );
            let app_version = db
                .get_config("app_version")
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let plugins_root = data_dir_p.join("plugins");
            tokio::fs::create_dir_all(&plugins_root)
                .await
                .map_err(|error| {
                    EngineError::Plugin(format!(
                        "create plugin directory {}: {error}",
                        plugins_root.display()
                    ))
                })?;
            let pm = Arc::new(plugin::PluginManager::with_audit(
                runtime,
                bridge,
                db.clone(),
                plugins_root,
                app_version,
                plugin_sink,
                audit,
            ));
            pm.load_all().await;
            manager.rss.set_fallback_provider(pm.subscription_router());
            manager.install_plugin_manager(pm, engine_bridge);
        }
        // 装载 RSS 订阅到内存镜像——放在引擎构造而非交给宿主，保证任何
        // 宿主（daemon / CLI --local）都不必记得这一步就能让轮询生效。
        manager.rss.load().await;
        // 同理装载 webhook 端点表：桌面 / headless / CLI --local 共享 config
        // 表，任何宿主都无需额外接线即可获得任务事件推送。
        manager.load_webhook_endpoints().await;
        // 回灌投递日志：面板在重启后仍能看到「昨晚那批到底发出去没有」。
        manager.webhook().attach_db(db.clone()).await;
        // 防吸血：恢复条目 / 账本 / strike。失败不阻断引擎启动（本次从空状态开始，错误已记日志）。
        #[cfg(feature = "bt")]
        if let Err(error) = manager.bt_guard().load().await {
            logger::report_warning("bt-guard", "load persisted state", &error);
        }
        Ok(Self {
            db,
            write_guard: Arc::new(write_guard),
            manager,
            selector,
            data_dir,
            activity_sink: sink,
            activity_journal,
        })
    }

    /// 唯一写入者租约的共享句柄；宿主心跳周期性调用其 `verify_lease()`，
    /// 租约被他人夺走时及时停机。
    pub fn write_guard(&self) -> Arc<db::EngineWriteGuard> {
        self.write_guard.clone()
    }

    /// 等待事件队列中的活动全部持久化；停机前调用以免内存尾部丢失。
    pub async fn flush_task_activity(&self) -> Result<(), String> {
        self.activity_journal.flush().await
    }
    /// 报告器与 actor 排空后仍可执行最终刷盘。
    pub fn activity_journal(&self) -> Arc<task_activity::JournalSink> {
        self.activity_journal.clone()
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
    /// 把“不阻塞宿主 runtime”的责任收进 Engine 内部。
    /// 解析本身是纯 CPU 计算(无网络),`spawn_blocking` 的
    /// `JoinError`(仅 panic 才会发生)转换为一条 `TorrentMetaResult.error`,
    /// 而不是让调用方处理一个额外的 `Result` 分支。
    pub async fn probe_torrent_meta(
        &self,
        probe_id: String,
        torrent_bytes: Vec<u8>,
    ) -> TorrentMetaResult {
        #[cfg(feature = "bt")]
        {
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
        // 无 BT 后端：明确回报不支持，而不是静默返回空结果。
        #[cfg(not(feature = "bt"))]
        {
            let _ = torrent_bytes;
            TorrentMetaResult {
                probe_id,
                name: String::new(),
                total_bytes: 0,
                files: Vec::new(),
                error: download_manager::BT_DISABLED_MESSAGE.to_owned(),
            }
        }
    }

    /// 投递日志快照（新的在前，上限 [`webhook::MAX_DELIVERY_LOG`]）。
    ///
    /// 日志的读路径是 [`webhook::WebhookDispatcher`] 的内存环形缓冲（启动时
    /// 从 `webhook_deliveries` 表回灌），宿主必须经此读取并转换为协议响应。
    pub fn webhook_deliveries(&self) -> Vec<webhook::WebhookDelivery> {
        self.manager.webhook().deliveries()
    }

    /// 清空投递日志（内存 + 落盘）。
    pub async fn clear_webhook_deliveries(&self) {
        self.manager.webhook().clear_deliveries().await;
    }

    /// 对**草稿**端点单发一次样例事件（不重试，用户在等内联反馈）。
    /// 端点无需先保存——「发送测试」在保存前就要能用。
    pub async fn test_webhook_endpoint(
        &self,
        spec: webhook::EndpointSpec,
    ) -> webhook::WebhookDelivery {
        self.manager.webhook().test_endpoint(spec).await
    }

    /// 「模拟一次 task.completed」：按已保存端点的订阅规则走完整投递路径。
    ///
    /// 返回投出去的端点数；0 = 没有端点订阅该事件（UI 该直说，别转圈）。
    pub fn simulate_webhook_event(&self) -> usize {
        self.manager.webhook().emit_sample()
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
