//! RSS 订阅自动下载（issue #97，设计文档 `docs/rss-subscription-design.md`）。
//!
//! # 形状
//!
//! 与 BT tracker 订阅（`tracker_subscription`，仅 `bt` feature）、插件惰性解析同款的
//! 「**actor tick → 纯函数判定 → off-actor 抓取 → mpsc 回流 → actor 落库**」
//! 结构：
//!
//! ```text
//! 宿主 actor tick ──▶ RssManager::tick()
//!                        ├─ due_sources()           纯函数，可单测
//!                        └─ spawn 抓取（off-actor）── HTTP + feed 解析
//!                                                      │
//!                        ┌─────────────────────────────┘ mpsc 回流
//!                        ▼
//!  DownloadManager::on_rss_event()
//!        ├─ RssManager::apply_fetch()   去重/过滤/落库/状态机（不建任务）
//!        └─ create_task(NewTaskSpec)    任务创建的唯一收敛点
//! ```
//!
//! **抓取必须 off-actor**：宿主 actor 跑在 `current_thread` runtime 上，网络
//! IO 直接 `await` 在事件循环里会冻住整个 App。
//!
//! # 三层去重
//!
//! 1. `guid`（[`parser`] 的回退链）——同一条目永不重复入库；
//! 2. 单轮上限 `max_per_fetch`——超额条目留在 [`RssItemStatus::New`]，下一轮
//!    按发布时间**从旧到新**继续派发，不丢也不插队；
//! 3. 智能剧集去重（可选）——同源同集只下一个字幕组版本，**识别失败即放行**。

pub mod filter;
pub mod model;
pub mod parser;
pub mod preview;

pub use model::RSS_PROVIDER_ID;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::auto_proxy::{self, CandidateSource};
use crate::db::Db;
use crate::downloader;
use crate::events::{EngineEvent, EventSink};
use crate::logger::log_error;
use crate::proxy_config::{ProxyConfig, ProxyMode};
use crate::rss::filter::{CompiledRule, FilterRule, Verdict};
use crate::rss::model::{RssItemInfo, RssItemStatus, RssSourceInfo};
use crate::rss::parser::{MAX_FEED_BYTES, ParsedFeed, parse_feed_at};
use crate::rule_route::EgressRouter;
use crate::subscription::{SubscriptionFetchRequest, SubscriptionProvider};

/// 每源保留的条目上限（超量淘汰最旧的**非已下载**条目）。
pub const MAX_ITEMS_PER_SOURCE: i32 = 500;
/// 失败退避的封顶间隔（6 小时）。
pub const MAX_BACKOFF_SECS: i64 = 6 * 3600;
/// 单次 feed 抓取的超时。
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
static FEED_PARSERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
/// 单个 `.torrent` 文件的大小上限（4 MiB）。真实种子几 KB 到几百 KB；
/// 超过这个量级的响应基本可以断定不是种子（登录页 / 误配的直链）。
const MAX_TORRENT_BYTES: usize = 4 * 1024 * 1024;
/// 首轮抓取的历史条目所带的原因码。
pub const REASON_SEED_SKIPPED: &str = "seed_skipped";
/// 种子抓取失败时写入条目 `reason` 的稳定码（条目保持 New，派发时排后并进入退避）。
pub const REASON_TORRENT_FETCH_FAILED: &str = "torrent_fetch_failed";
/// 种子抓取失败后首次退避时长（秒），此后每次失败翻倍。
const TORRENT_RETRY_BASE_SECS: i64 = 600;
/// 种子抓取退避时长上限（秒）。
const TORRENT_RETRY_MAX_SECS: i64 = 24 * 60 * 60;
/// 种子的标准 MIME 类型。
const TORRENT_MIME: &str = "application/x-bittorrent";

/// 第 `failures` 次连续抓取失败后的退避秒数：`base × 2^(failures-1)`，封顶 24 小时。
pub(crate) fn torrent_retry_delay_secs(failures: i64) -> i64 {
    let shift = u32::try_from((failures - 1).clamp(0, 30)).unwrap_or(0);
    TORRENT_RETRY_BASE_SECS
        .saturating_mul(1i64 << shift)
        .min(TORRENT_RETRY_MAX_SECS)
}

/// off-actor 抓取的回流结果。
#[derive(Debug)]
pub struct RssFetchOutcome {
    /// 发起抓取的订阅。
    pub source_id: String,
    /// 解析出的 feed（`error` 非空时为默认值）。
    pub feed: ParsedFeed,
    /// 失败原因（空 = 成功）。
    pub error: String,
}

/// 新建订阅向导第二步的只读验证结果（不落库、不建任务）。
#[derive(Debug)]
pub struct RssValidateOutcome {
    /// 调用方给的请求 ID，用于把结果配回发起的对话框。
    pub request_id: String,
    /// 被验证的 feed 地址。
    pub url: String,
    /// feed 标题（供回填订阅名）。
    pub feed_title: String,
    /// 条目预览（`source_id` 为空的瞬态 [`RssItemInfo`]）。
    pub items: Vec<RssItemInfo>,
    /// 失败原因（空 = 验证通过）。
    pub error: String,
}

/// 「立即抓取」的派发结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RssRefreshOutcome {
    /// 已派发一次新的抓取。
    Started,
    /// 该订阅已有抓取在途——幂等，结果会经正常回流到达，无需重复触发。
    AlreadyRunning,
    /// 订阅不存在。
    NotFound,
}

/// off-actor worker 回流到 actor 的两类结果。
#[derive(Debug)]
pub enum RssEvent {
    /// 定时/手动抓取完成。
    Fetched(Box<RssFetchOutcome>),
    /// 新建向导的 feed 验证完成。
    Validated(Box<RssValidateOutcome>),
    /// `.torrent` 字节抓取完成——BT 条目建任务的第二段（见
    /// [`RssDownloadPlan::is_torrent_file`]）。
    TorrentReady(Box<RssTorrentOutcome>),
}

/// 为一个 BT 条目抓取 `.torrent` 字节的结果。
#[derive(Debug)]
pub struct RssTorrentOutcome {
    /// 原样带回的建任务指令。
    pub plan: Box<RssDownloadPlan>,
    /// 种子文件内容（`error` 非空时为空）。
    pub bytes: Vec<u8>,
    /// 失败原因（空 = 成功）。
    pub error: String,
}

/// 一条「应当为该条目建任务」的指令。
///
/// [`RssManager`] 只做判定与落库，**不碰任务创建**——建任务必须收敛到
/// [`crate::download_manager::DownloadManager::create_task`] 这唯一入口，
/// 所以判定结果以指令形式交回 `DownloadManager` 执行。
#[derive(Debug, Clone, Default)]
pub struct RssDownloadPlan {
    /// 来源订阅。
    pub source_id: String,
    /// 条目 guid（建完任务后据此回写 `Downloaded` + `task_id`）。
    pub guid: String,
    /// 条目标题（通知文案）。
    pub title: String,
    /// 下载地址；带 resolverItem 时使用条目链接触发二段解析。
    pub url: String,
    /// 插件二段解析标识（空 = 普通 RSS 直链）。
    pub resolver_item: String,
    /// 订阅配置的保存目录（空 = 由调用方按 队列目录 → 全局目录 兜底）。
    pub save_dir: String,
    /// 目标队列（空 = 主队列）。
    pub queue_id: String,
    /// 是否以 paused 落库。
    pub start_paused: bool,
    /// 请求 Cookie。
    pub cookies: String,
    /// 订阅级 UA（空 = 队列/全局）。
    pub user_agent: String,
    /// 订阅级代理（空 = 全局）。
    pub proxy_url: String,
    /// Referer（`send_referer` 关时为空）。
    pub referrer: String,
    /// enclosure 声明大小（0 = 未知）。**仅供展示，绝不当作 `hint_file_size`**：
    /// BT feed 的 `<enclosure length>` 描述的是种子**内容**总大小（几百 MB 的
    /// 番剧），而 enclosure 本身只是几 KB 的 `.torrent`。拿它当文件大小 hint
    /// 会让引擎按几百 MB 规划多段并发并跳过 probe——首段立刻 truncated、后续
    /// 段全 416，还会把站点误学成「只支持 2 连接」污染域名策略缓存。
    pub size_hint: i64,
    /// 该订阅是否开启「自动下载时通知」。
    pub notify: bool,
    /// 下载目标声明的 MIME 类型（小写、不含参数；空 = 未声明，或二段解析条目）。
    /// `application/x-bittorrent` 视为种子，不要求 URL 带 `.torrent` 扩展名。
    pub enclosure_type: String,
}

impl RssDownloadPlan {
    /// 下载地址是否指向一个 `.torrent` 文件。
    ///
    /// 引擎的 BT 判定（`is_bt_url`）只认 `magnet:` 与 `torrent-file://` 哨兵，
    /// **HTTP 的 `.torrent` 直链会被当成普通文件下载**——那样订阅 Mikan 只会
    /// 攒下一堆 `.torrent`，番剧本体一个都不下。命中本判定的条目要先把种子
    /// 字节抓下来，再以 `torrent_file_bytes` 建真正的 BT 任务。
    ///
    /// # Examples
    ///
    /// ```
    /// use fluxdown_engine::rss::RssDownloadPlan;
    ///
    /// let plan = |url: &str| RssDownloadPlan { url: url.to_string(), ..Default::default() };
    ///
    /// assert!(plan("https://mikanani.me/Download/20260727/abc.torrent").is_torrent_file());
    /// // 带 query / 大小写混排照样认
    /// assert!(plan("https://pt.example/dl?id=1&file=x.TORRENT").is_torrent_file());
    /// // magnet 由引擎既有五路分派直接处理，不走种子抓取
    /// assert!(!plan("magnet:?xt=urn:btih:deadbeef").is_torrent_file());
    /// assert!(!plan("https://cdn.example/ep01.mp4").is_torrent_file());
    /// // PT / Jackett 的无扩展名下载链接：靠 feed 声明的 MIME 识别
    /// let typed = RssDownloadPlan {
    ///     url: "https://pt.example/download?id=1".to_string(),
    ///     enclosure_type: "application/x-bittorrent".to_string(),
    ///     ..Default::default()
    /// };
    /// assert!(typed.is_torrent_file());
    /// ```
    #[must_use]
    pub fn is_torrent_file(&self) -> bool {
        if crate::bt_config::is_magnet_url(&self.url) {
            return false;
        }
        self.enclosure_type == TORRENT_MIME || url_looks_like_torrent(&self.url)
    }
}

/// URL 形态判定（见 [`RssDownloadPlan::is_torrent_file`]）。除 `.torrent`
/// 字样外，认两种不含扩展名的常见种子下载形态：NexusPHP 的
/// `download.php?…passkey=…` 与 Jackett 的 `/dl/<indexer>/?jackett_apikey=…`。
fn url_looks_like_torrent(url: &str) -> bool {
    if crate::bt_config::is_magnet_url(url) {
        return false;
    }
    let lowered = url.to_ascii_lowercase();
    let (path, query) = {
        let no_fragment = lowered.split('#').next().unwrap_or(&lowered);
        match no_fragment.split_once('?') {
            Some((p, q)) => (p, q),
            None => (no_fragment, ""),
        }
    };
    // 少数 PT 站把真实文件名放进 query（`…/dl.php?file=x.torrent`），一并认。
    if path.ends_with(".torrent") || lowered.contains(".torrent") {
        return true;
    }
    let has_param = |key: &str| {
        query
            .split('&')
            .any(|kv| kv.split('=').next().is_some_and(|k| k == key))
    };
    (path.ends_with("/download.php") && has_param("passkey"))
        || (path.contains("/dl/") && has_param("jackett_apikey"))
}

/// 订阅调度与条目状态机。
pub struct RssManager {
    db: Db,
    sink: Arc<dyn EventSink>,
    /// 内存镜像（顺序即 `position`）。DB 是事实源，这里避免 tick 每 20 秒扫表。
    sources: Vec<RssSourceInfo>,
    /// 正在抓取中的订阅——防同一源被 tick 与手动刷新重复派发。
    in_flight: HashSet<String>,
    /// 订阅来源适配器。RSS 是内置 provider，插件可以注册自己的 provider。
    providers: HashMap<String, Arc<dyn SubscriptionProvider>>,
    /// 未命中内置 map 时交给宿主提供的动态 provider（插件路由）。
    fallback_provider: Option<Arc<dyn SubscriptionProvider>>,
    tx: mpsc::UnboundedSender<RssEvent>,
    rx: Option<mpsc::UnboundedReceiver<RssEvent>>,
}

impl RssManager {
    /// 构造（不读库；由 [`RssManager::load`] 装载）。
    pub fn new(db: Db, sink: Arc<dyn EventSink>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut providers: HashMap<String, Arc<dyn SubscriptionProvider>> = HashMap::new();
        providers.insert(RSS_PROVIDER_ID.to_string(), Arc::new(BuiltinRssProvider));
        Self {
            db,
            sink,
            sources: Vec::new(),
            in_flight: HashSet::new(),
            providers,
            fallback_provider: None,
            tx,
            rx: Some(rx),
        }
    }

    /// 交出回流接收端给宿主 actor 的事件循环（同 `take_resolve_rx` 惯例，
    /// 只能取一次）。
    pub fn take_event_rx(&mut self) -> Option<mpsc::UnboundedReceiver<RssEvent>> {
        self.rx.take()
    }

    /// 从 DB 装载全部订阅到内存镜像。由 [`crate::Engine::new`] 调用，宿主无需
    /// 记得这一步。
    ///
    /// 重新装载时，已在内存里的订阅其**运行态列**（抓取时间戳 / 错误 / 退避 /
    /// 首轮标记）以内存为准：无语义变化的抓取只更新内存不落库（见
    /// [`RssManager::record_runtime`]），DB 里的运行态可能偏旧，直接覆盖会把
    /// due 判定回退成「立刻再抓一次」。其余字段（用户可编辑列、未读数）以 DB
    /// 为准。
    pub async fn load(&mut self) {
        match self.db.load_all_rss_sources().await {
            Ok(mut fresh) => {
                merge_runtime_from_memory(&mut fresh, &self.sources);
                self.sources = fresh;
            }
            Err(e) => log_error!("[rss] failed to load sources: {}", e),
        }
    }

    /// 当前订阅列表（宿主快照请求 / UI 首帧）。
    pub fn sources(&self) -> &[RssSourceInfo] {
        &self.sources
    }

    /// 按 ID 取订阅。
    pub fn source(&self, source_id: &str) -> Option<&RssSourceInfo> {
        self.sources.iter().find(|s| s.source_id == source_id)
    }

    /// 设置未命中固定 provider map 时使用的动态路由器。
    pub fn set_fallback_provider(&mut self, provider: Arc<dyn SubscriptionProvider>) {
        self.fallback_provider = Some(provider);
    }

    /// 广播订阅列表（含未读计数，重新读库以刷新 badge）。
    pub async fn broadcast_sources(&mut self) {
        self.load().await;
        self.emit_sources_snapshot();
    }

    /// 直接用内存镜像推订阅列表快照，不读库。
    fn emit_sources_snapshot(&self) {
        self.sink
            .emit(EngineEvent::RssSourcesChanged(self.sources.clone()));
    }

    /// 广播某订阅的条目流快照。
    pub async fn broadcast_items(&self, source_id: &str, notify_titles: Vec<String>) {
        let items = self
            .db
            .load_rss_items(source_id, MAX_ITEMS_PER_SOURCE)
            .await
            .unwrap_or_default();
        self.sink.emit(EngineEvent::RssItemsChanged {
            source_id: source_id.to_string(),
            items,
            notify_titles,
        });
    }

    // -----------------------------------------------------------------------
    // CRUD
    // -----------------------------------------------------------------------

    /// 新建订阅。`source.source_id` 为空时自动生成 UUID；返回最终 ID。
    ///
    /// 新订阅的 `last_fetch_at = 0`，因此下一次 tick 立刻抓取——首轮的全部
    /// 历史条目只标记 [`RssItemStatus::SeedSkipped`] 不下载（§2.2）。
    pub async fn create_source(&mut self, mut source: RssSourceInfo) -> Option<String> {
        source.normalize();
        if source.url.is_empty() {
            return None;
        }
        if source.source_id.is_empty() {
            source.source_id = uuid::Uuid::new_v4().to_string();
        }
        source.position = self.db.next_rss_position().await.unwrap_or(0);
        source.last_fetch_at = 0;
        source.seeded = false;
        if let Err(e) = self.db.insert_rss_source(&source).await {
            log_error!("[rss] insert source failed: {}", e);
            return None;
        }
        let id = source.source_id.clone();
        tracing::info!(source_id = %id, "RSS subscription created");
        self.broadcast_sources().await;
        Some(id)
    }

    /// 更新订阅的用户可编辑字段。运行态（退避账本/首轮标记）不受影响。
    pub async fn update_source(&mut self, mut source: RssSourceInfo) -> bool {
        source.normalize();
        let Some(old) = self.source(&source.source_id).cloned() else {
            return false;
        };

        if let Err(e) = self.db.update_rss_source(&source).await {
            log_error!("[rss] update source failed: {}", e);
            return false;
        }
        if let Some(seeded) = runtime_reset_on_update(&old, &source) {
            // 来源/鉴权变化：旧的失败退避与错误不再适用，`last_fetch_at = 0`
            // 让下一次 tick 立即重抓（与新建路径一致）。
            self.persist_runtime(
                &source.source_id,
                &RuntimeUpdate {
                    last_fetch_at: 0,
                    last_success_at: old.last_success_at,
                    last_error: String::new(),
                    fail_count: 0,
                    seeded,
                    name: source.name.clone(),
                },
            )
            .await;
        }
        self.broadcast_sources().await;
        true
    }

    /// 删除订阅（级联条目；已创建的下载任务保留）。
    pub async fn delete_source(&mut self, source_id: &str) -> bool {
        if self.source(source_id).is_none() {
            return false;
        }
        if let Err(e) = self.db.delete_rss_source(source_id).await {
            log_error!("[rss] delete source failed: {}", e);
            return false;
        }
        self.in_flight.remove(source_id);
        self.broadcast_sources().await;
        true
    }

    /// 把该源全部「新」条目标记为已读。
    pub async fn mark_all_read(&mut self, source_id: &str) {
        if let Err(e) = self.db.mark_all_rss_items_read(source_id).await {
            log_error!("[rss] mark all read failed: {}", e);
            return;
        }
        self.broadcast_items(source_id, Vec::new()).await;
        self.broadcast_sources().await;
    }

    /// 手动忽略一个条目。
    pub async fn ignore_item(&mut self, source_id: &str, guid: &str) {
        if let Err(e) = self
            .db
            .set_rss_item_status(source_id, guid, RssItemStatus::Ignored, "", "")
            .await
        {
            log_error!("[rss] ignore item failed: {}", e);
            return;
        }
        self.broadcast_items(source_id, Vec::new()).await;
        self.broadcast_sources().await;
    }

    /// 手动下载一个条目（「仍要下载」/「补下」/「重新下载」——绕过规则与
    /// 剧集去重）。
    ///
    /// **任何状态都允许**，包括已下载：任务可能被用户删了、下到一半失败了、
    /// 或者只是想再来一遍。挡住重下没有任何好处，只会逼用户去别处找种子。
    /// 重下会覆盖旧的 `task_id` 回链，指向新任务。
    ///
    /// 条目或订阅不存在时返回 `None`。
    pub async fn manual_download(
        &self,
        source_id: &str,
        guid: &str,
    ) -> Option<Box<RssDownloadPlan>> {
        let source = self.source(source_id)?;
        let item = self.db.rss_item(source_id, guid).await.ok().flatten()?;
        Some(Box::new(plan_for(source, &item)))
    }

    // -----------------------------------------------------------------------
    // 调度
    // -----------------------------------------------------------------------

    /// 定时节拍：派发全部到期订阅的抓取。宿主 actor 每次 tick 调用一次。
    pub fn tick(&mut self, now: i64, proxy: &ProxyConfig, global_ua: &str) {
        let due = due_sources(now, self.sources.iter(), &self.in_flight);
        for id in due {
            self.dispatch_fetch(&id, now, proxy, global_ua);
        }
    }

    /// 立即抓取一个订阅（侧边栏「立即刷新」/ REST `POST /rss/{id}/refresh`）。
    ///
    /// 忽略 due 判定但仍尊重 `in_flight`——连点刷新不该并发打同一个站点，
    /// 此时返回 [`RssRefreshOutcome::AlreadyRunning`]（幂等，不是错误）。
    pub fn refresh_now(
        &mut self,
        source_id: &str,
        proxy: &ProxyConfig,
        global_ua: &str,
    ) -> RssRefreshOutcome {
        if self.source(source_id).is_none() {
            return RssRefreshOutcome::NotFound;
        }
        if self.in_flight.contains(source_id) {
            return RssRefreshOutcome::AlreadyRunning;
        }
        self.dispatch_fetch(source_id, unix_now(), proxy, global_ua);
        RssRefreshOutcome::Started
    }

    fn dispatch_fetch(&mut self, source_id: &str, now: i64, proxy: &ProxyConfig, global_ua: &str) {
        let Some(source) = self.source(source_id) else {
            return;
        };
        let provider_id = source.provider_id.clone();
        let base_request = FetchRequest {
            source_id: source.source_id.clone(),
            url: source.url.clone(),
            provider_id: provider_id.clone(),
            provider_config: source.provider_config.clone(),
            cookies: source.cookies.clone(),
            user_agent: if source.user_agent.is_empty() {
                global_ua.to_string()
            } else {
                source.user_agent.clone()
            },
            proxy_url: source.proxy_url.clone(),
            proxy: ProxyConfig::default(),
            egress: None,
        };
        let route = resolve_route(
            &source.proxy_url,
            proxy,
            &source.url,
            &base_request.user_agent,
        );
        let direct_proxy = route.proxy;
        let route_egress = route.egress;
        // ProxyConfig::resolve() 把非 coordinator 路径的 Auto 折算成
        // 直连（见该函数文档），只有 HTTP 下载 coordinator 消费 auto_proxy
        // 的采样/决策。RSS 抓取独立于 coordinator，因此这里直连失败时按
        // 「手动代理 → 系统代理」候选顺序手动重试一遍。只对内置 RSS provider
        // 启用：插件 provider 由插件层按订阅代理 / 站点规则 / Auto 计划自行换路
        // （见 [`SubscriptionFetchRequest::proxy_url`]），这里再套一层会重复抓取。
        let eligible = source.proxy_url.is_empty()
            && proxy.mode == ProxyMode::Auto
            && !route.rule_hit
            && provider_id == RSS_PROVIDER_ID;
        let global_proxy = proxy.clone();
        let provider = self
            .providers
            .get(&provider_id)
            .cloned()
            .or_else(|| self.fallback_provider.clone());
        // 乐观置位 last_fetch_at：即便抓取任务本身崩了，due 判定也不会把这个
        // 源变成每 tick 重试的死循环（回流分支会用真实结果覆盖）。
        self.in_flight.insert(base_request.source_id.clone());
        if let Some(s) = self
            .sources
            .iter_mut()
            .find(|s| s.source_id == base_request.source_id)
        {
            s.last_fetch_at = now;
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let source_id = base_request.source_id.clone();
            let builtin = provider_id == RSS_PROVIDER_ID;
            let provider_available = provider.is_some();
            let started = std::time::Instant::now();
            let outcome = match provider {
                Some(provider) => {
                    let result = fetch_with_auto_failover(
                        direct_proxy,
                        eligible,
                        &global_proxy,
                        |proxy_cfg| {
                            let mut req = base_request.clone();
                            req.proxy = proxy_cfg;
                            req.egress = route_egress.clone();
                            provider.fetch(req)
                        },
                    )
                    .await;
                    match result {
                        Ok(feed) => RssFetchOutcome {
                            source_id,
                            feed,
                            error: String::new(),
                        },
                        Err(error) => RssFetchOutcome {
                            source_id,
                            feed: ParsedFeed::default(),
                            error,
                        },
                    }
                }
                None => RssFetchOutcome {
                    source_id,
                    feed: ParsedFeed::default(),
                    error: format!("subscription provider not installed: {provider_id}"),
                },
            };
            tracing::debug!(
                source_id = %outcome.source_id,
                builtin,
                provider_available,
                success = outcome.error.is_empty(),
                items = outcome.feed.items.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "RSS provider poll finished"
            );
            if tx.send(RssEvent::Fetched(Box::new(outcome))).is_err() {
                tracing::debug!("RSS fetch owner stopped before result delivery");
            }
        });
    }

    /// 构造一个只读验证任务（新建向导 / REST `POST /rss/validate`）。
    ///
    /// 返回 future 而不是自己 spawn：actor 侧的调用方（信号路径）直接
    /// [`tokio::spawn`] 走事件广播；请求-应答的调用方（REST/CLI）在 actor 之外
    /// `await` 它拿返回值。两条路共用同一段抓取+解析逻辑。
    ///
    /// `provider_id` 与订阅抓取同一路由：内置 `rss` 走 [`BuiltinRssProvider`]，
    /// 其它 ID 走固定 map 或宿主注入的插件路由；空值按内置 RSS 处理。
    #[allow(clippy::too_many_arguments)]
    pub fn validate_future(
        &self,
        request_id: String,
        provider_id: String,
        provider_config: String,
        url: String,
        cookies: String,
        user_agent: String,
        proxy_url: String,
        proxy: &ProxyConfig,
        global_ua: &str,
    ) -> impl Future<Output = RssValidateOutcome> + Send + use<> {
        let provider_id = if provider_id.trim().is_empty() {
            RSS_PROVIDER_ID.to_string()
        } else {
            provider_id.trim().to_string()
        };
        let base_request = FetchRequest {
            source_id: String::new(),
            provider_id: provider_id.clone(),
            url: url.clone(),
            provider_config: provider_config.trim().to_string(),
            cookies,
            user_agent: if user_agent.is_empty() {
                global_ua.to_string()
            } else {
                user_agent
            },
            proxy_url: proxy_url.clone(),
            proxy: ProxyConfig::default(),
            egress: None,
        };
        let route = resolve_route(&proxy_url, proxy, &url, &base_request.user_agent);
        let direct_proxy = route.proxy;
        let route_egress = route.egress;
        // 新建订阅向导/REST 验证同样只拿到折算后的直连配置，Auto 模式下
        // 补一次候选代理重试，避免把「直连探测失败」误判为订阅地址本身
        // 不可用（用户此前需手动切到 System 模式才能通过验证）。插件 provider
        // 由插件层自行换路，与 `dispatch_fetch` 一样不在这里做失败转移。
        let eligible = proxy_url.is_empty()
            && proxy.mode == ProxyMode::Auto
            && !route.rule_hit
            && provider_id == RSS_PROVIDER_ID;
        let global_proxy = proxy.clone();
        let provider = self
            .providers
            .get(&provider_id)
            .cloned()
            .or_else(|| self.fallback_provider.clone());
        async move {
            let builtin = provider_id == RSS_PROVIDER_ID;
            let provider_available = provider.is_some();
            let started = std::time::Instant::now();
            let result = match provider {
                Some(provider) => {
                    fetch_with_auto_failover(direct_proxy, eligible, &global_proxy, |proxy_cfg| {
                        let mut req = base_request.clone();
                        req.proxy = proxy_cfg;
                        req.egress = route_egress.clone();
                        provider.fetch(req)
                    })
                    .await
                }
                None => Err(format!(
                    "subscription provider not installed: {provider_id}"
                )),
            };
            tracing::debug!(
                builtin,
                provider_available,
                success = result.is_ok(),
                items = result.as_ref().map_or(0, |feed| feed.items.len()),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "RSS provider validation finished"
            );
            match result {
                Ok(feed) => RssValidateOutcome {
                    request_id,
                    url,
                    feed_title: feed.title,
                    items: feed
                        .items
                        .iter()
                        .map(|it| item_from_parsed("", it, 0))
                        .collect(),
                    error: String::new(),
                },
                Err(error) => RssValidateOutcome {
                    request_id,
                    url,
                    feed_title: String::new(),
                    items: Vec::new(),
                    error,
                },
            }
        }
    }

    /// 信号路径的验证：off-actor 跑 [`Self::validate_future`]，结果经回流通道
    /// 广播为 [`EngineEvent::RssFeedValidated`]。
    #[allow(clippy::too_many_arguments)]
    pub fn validate(
        &self,
        request_id: String,
        provider_id: String,
        provider_config: String,
        url: String,
        cookies: String,
        user_agent: String,
        proxy_url: String,
        proxy: &ProxyConfig,
        global_ua: &str,
    ) {
        let fut = self.validate_future(
            request_id,
            provider_id,
            provider_config,
            url,
            cookies,
            user_agent,
            proxy_url,
            proxy,
            global_ua,
        );
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if tx.send(RssEvent::Validated(Box::new(fut.await))).is_err() {
                tracing::debug!("RSS validation owner stopped before result delivery");
            }
        });
    }

    /// 为一个 BT 条目 off-actor 抓取 `.torrent` 字节，结果经回流通道回到
    /// [`crate::download_manager::DownloadManager::on_rss_event`]。
    ///
    /// 这是 BT 条目建任务的**第二段**：第一段（feed 抓取 + 规则判定）产出
    /// plan，这里把种子内容拿到手，最后才在 actor 上以 `torrent_file_bytes`
    /// 调 `create_task`。分两段是因为「哪些条目该下」要查 DB 去重（actor 上
    /// 才能做），而网络 IO 绝不能上 actor。
    pub fn spawn_torrent_fetch(
        &self,
        plan: Box<RssDownloadPlan>,
        proxy: &ProxyConfig,
        global_ua: &str,
    ) {
        let request = FetchRequest {
            source_id: plan.source_id.clone(),
            provider_id: RSS_PROVIDER_ID.to_string(),
            url: plan.url.clone(),
            provider_config: String::new(),
            cookies: plan.cookies.clone(),
            user_agent: if plan.user_agent.is_empty() {
                global_ua.to_string()
            } else {
                plan.user_agent.clone()
            },
            proxy_url: plan.proxy_url.clone(),
            proxy: ProxyConfig::default(),
            egress: None,
        };
        let referrer = plan.referrer.clone();
        // 与 feed 抓取同一套 Auto 失败转移：feed 靠候选代理才抓得到的站点，
        // 其 `.torrent` 下载同样需要，否则整条 RSS→BT 链路在 Auto 下不可用。
        let route = resolve_route(&plan.proxy_url, proxy, &plan.url, &request.user_agent);
        let direct_proxy = route.proxy;
        let route_egress = route.egress;
        let eligible =
            plan.proxy_url.is_empty() && proxy.mode == ProxyMode::Auto && !route.rule_hit;
        let global_proxy = proxy.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result =
                fetch_with_auto_failover(direct_proxy, eligible, &global_proxy, |proxy_cfg| {
                    let mut req = request.clone();
                    req.proxy = proxy_cfg;
                    req.egress = route_egress.clone();
                    let referrer = referrer.clone();
                    async move { fetch_torrent(&req, &referrer).await }
                })
                .await;
            let (bytes, error) = match result {
                Ok(bytes) => (bytes, String::new()),
                Err(e) => (Vec::new(), e),
            };
            if tx
                .send(RssEvent::TorrentReady(Box::new(RssTorrentOutcome {
                    plan,
                    bytes,
                    error,
                })))
                .is_err()
            {
                tracing::debug!("RSS torrent owner stopped before result delivery");
            }
        });
    }

    /// 广播验证结果。
    pub fn emit_validated(&self, outcome: RssValidateOutcome) {
        self.sink.emit(EngineEvent::RssFeedValidated {
            request_id: outcome.request_id,
            url: outcome.url,
            feed_title: outcome.feed_title,
            items: outcome.items,
            error: outcome.error,
        });
    }

    // -----------------------------------------------------------------------
    // 回流处理
    // -----------------------------------------------------------------------

    /// 消化一次抓取结果：去重 → 过滤判定 → 落库 → 选出应下载的条目。
    ///
    /// 返回的 [`RssDownloadPlan`] 由 `DownloadManager` 逐条走 `create_task`，
    /// 成功后回调 [`RssManager::mark_downloaded`]。
    pub async fn apply_fetch(&mut self, outcome: RssFetchOutcome) -> Vec<RssDownloadPlan> {
        let now = unix_now();
        self.in_flight.remove(&outcome.source_id);
        // 订阅可能在抓取窗口内被删除——静默丢弃，不建表也不建任务。
        let Some(source) = self.source(&outcome.source_id).cloned() else {
            return Vec::new();
        };

        if !outcome.error.is_empty() {
            let fail_count = source.fail_count.saturating_add(1);
            if fail_count == 2 {
                tracing::warn!(source_id = %source.source_id, fail_count, "RSS polling degraded after consecutive failures");
            } else {
                tracing::debug!(source_id = %source.source_id, fail_count, "RSS poll failed; background polling will retry");
            }
            let persisted = self
                .record_runtime(
                    &source.source_id,
                    RuntimeUpdate {
                        last_fetch_at: now,
                        last_success_at: source.last_success_at,
                        last_error: outcome.error.clone(),
                        fail_count,
                        seeded: source.seeded,
                        name: source.name.clone(),
                    },
                )
                .await;
            if persisted {
                self.broadcast_sources().await;
            } else {
                self.emit_sources_snapshot();
            }
            return Vec::new();
        }

        let first_round = !source.seeded;
        let known = self
            .db
            .rss_known_guids(&source.source_id)
            .await
            .unwrap_or_default();
        let mut taken = if source.smart_episode {
            self.db
                .rss_taken_episode_keys(&source.source_id)
                .await
                .unwrap_or_default()
        } else {
            HashSet::new()
        };
        let compiled = CompiledRule::new(&rule_of(&source));

        let mut rows: Vec<RssItemInfo> = Vec::new();
        for parsed in &outcome.feed.items {
            if parsed.guid.is_empty() || known.contains(&parsed.guid) {
                continue;
            }
            let mut row = item_from_parsed(&source.source_id, parsed, now);
            if first_round {
                // 首轮：全部标记已读，只展示不下载。仍记录剧集键，让后续新
                // 条目能与历史正确去重。
                row.status = RssItemStatus::SeedSkipped;
                row.reason = REASON_SEED_SKIPPED.to_string();
                if source.smart_episode {
                    row.episode_key = filter::episode_key(&row.title).unwrap_or_default();
                }
            } else {
                match compiled.evaluate(&row.title, row.enclosure_length, &mut taken) {
                    Verdict::Accept { episode_key } => {
                        row.episode_key = episode_key;
                    }
                    Verdict::Reject {
                        reason,
                        episode_key,
                    } => {
                        row.status = reason.item_status();
                        row.reason = reason.code().to_string();
                        row.episode_key = episode_key;
                    }
                }
            }
            rows.push(row);
        }

        // 已知条目不再入库，但解析器可能这一版才补上它们的发布时间（Mikan 的
        // `<torrent><pubDate>`）——单独回填一次，历史行不必等被 prune 掉才有时间。
        let backfill: Vec<(String, i64)> = outcome
            .feed
            .items
            .iter()
            .filter(|p| p.pub_date > 0 && known.contains(&p.guid))
            .map(|p| (p.guid.clone(), p.pub_date))
            .collect();
        let mut backfilled = 0u64;
        if !backfill.is_empty() {
            match self
                .db
                .backfill_rss_pub_dates(&source.source_id, &backfill)
                .await
            {
                Ok(n) => backfilled = n,
                Err(e) => log_error!("[rss] backfill pub_date failed: {}", e),
            }
        }

        let resolver_backfill: Vec<(String, String)> = outcome
            .feed
            .items
            .iter()
            .filter(|p| !p.resolver_item.is_empty() && known.contains(&p.guid))
            .map(|p| (p.guid.clone(), p.resolver_item.clone()))
            .collect();
        let mut resolver_backfilled = 0u64;
        if !resolver_backfill.is_empty() {
            match self
                .db
                .backfill_rss_resolver_items(&source.source_id, &resolver_backfill)
                .await
            {
                Ok(n) => resolver_backfilled = n,
                Err(e) => log_error!("[rss] backfill resolver_item failed: {}", e),
            }
        }

        let image_backfill: Vec<_> = outcome
            .feed
            .items
            .iter()
            .filter(|item| !item.image_urls.is_empty() && known.contains(&item.guid))
            .map(|item| (item.guid.as_str(), item.image_urls.as_slice()))
            .collect();
        let images_backfilled = match self
            .db
            .backfill_rss_images(&source.source_id, &image_backfill)
            .await
        {
            Ok(changed) => changed,
            Err(error) => {
                log_error!("[rss] backfill image URLs failed: {error:#}");
                false
            }
        };

        let fresh = rows.len();
        if let Err(e) = self.db.insert_rss_items(&rows).await {
            log_error!("[rss] persist items failed: {}", e);
        }
        // 没有新条目就不会超量，省掉一次空 DELETE（仍要扫 rss_items 索引）。
        if fresh > 0
            && let Err(e) = self
                .db
                .prune_rss_items(&source.source_id, MAX_ITEMS_PER_SOURCE)
                .await
        {
            log_error!("[rss] prune items failed: {}", e);
        }

        // 订阅名留空时用 feed 标题回填（只回填一次，之后以用户改名为准）。
        let name = if source.name.is_empty() && !outcome.feed.title.is_empty() {
            outcome.feed.title.clone()
        } else {
            source.name.clone()
        };
        let persisted = self
            .record_runtime(
                &source.source_id,
                RuntimeUpdate {
                    last_fetch_at: now,
                    last_success_at: now,
                    last_error: String::new(),
                    fail_count: 0,
                    seeded: true,
                    name,
                },
            )
            .await;
        if persisted && source.fail_count >= 2 {
            tracing::info!(source_id = %source.source_id, "RSS polling recovered");
        }

        // 派发：`New` 状态的条目按发布时间从旧到新取，单轮不超过上限。
        // 首轮不派发（本轮全部是 SeedSkipped，但历史遗留的 New 也不该在
        // seeding 这一轮被突然灌下去）。
        let plans = if source.auto_download && !first_round {
            self.db
                .rss_dispatchable_items(&source.source_id, source.max_per_fetch, now)
                .await
                .unwrap_or_default()
                .iter()
                .map(|item| plan_for(&source, item))
                .collect()
        } else {
            Vec::new()
        };
        if first_round || fresh > 0 || !plans.is_empty() {
            tracing::info!(source_id = %source.source_id, first_round, fresh, planned = plans.len(), "RSS poll changed feed state");
        } else {
            tracing::debug!(source_id = %source.source_id, backfilled, resolver_backfilled, "RSS poll had no new or dispatchable items");
        }
        if fresh > 0
            || backfilled > 0
            || resolver_backfilled > 0
            || images_backfilled
            || !plans.is_empty()
        {
            self.broadcast_items(&source.source_id, Vec::new()).await;
        }
        if persisted || fresh > 0 || backfilled > 0 || resolver_backfilled > 0 || !plans.is_empty()
        {
            self.broadcast_sources().await;
        } else {
            // 纯时间戳前进：DB 没动，未读数也没变，直接从内存镜像推快照，
            // 既让 UI「上次检查」前进，又不必为此重读库。
            self.emit_sources_snapshot();
        }
        plans
    }

    /// 任务创建成功后回写条目状态与回链。
    pub async fn mark_downloaded(&self, source_id: &str, guid: &str, task_id: &str) {
        if let Err(e) = self
            .db
            .set_rss_item_status(source_id, guid, RssItemStatus::Downloaded, "", task_id)
            .await
        {
            log_error!("[rss] mark downloaded failed: {}", e);
        }
        self.clear_item_backoff(source_id, guid).await;
    }

    /// 种子抓取失败：条目保持 New 并带失败码，指数退避到期后才再参与自动派发
    /// （手动下载不受退避限制）。
    pub async fn record_torrent_failure(&self, source_id: &str, guid: &str) {
        match self
            .db
            .record_rss_fetch_failure(
                source_id,
                guid,
                REASON_TORRENT_FETCH_FAILED,
                unix_now(),
                torrent_retry_delay_secs,
            )
            .await
        {
            Ok(failures) => tracing::debug!(
                %source_id,
                failures,
                retry_secs = torrent_retry_delay_secs(failures),
                "RSS torrent fetch retry scheduled"
            ),
            Err(e) => log_error!("[rss] record torrent failure failed: {}", e),
        }
    }

    /// 清零条目的种子抓取失败计数与退避（建任务成功或用户手动下载时）。
    pub async fn clear_item_backoff(&self, source_id: &str, guid: &str) {
        if let Err(e) = self.db.clear_rss_item_backoff(source_id, guid).await {
            log_error!("[rss] clear item backoff failed: {}", e);
        }
    }

    /// 回写运行态并同步内存镜像（无条件落库）。用于编辑订阅后的重置等
    /// 必须让 DB 立即反映的场景；抓取回流走 [`Self::record_runtime`]。
    async fn persist_runtime(&mut self, source_id: &str, update: &RuntimeUpdate) {
        if let Err(e) = self
            .db
            .set_rss_source_runtime(
                source_id,
                update.last_fetch_at,
                update.last_success_at,
                &update.last_error,
                update.fail_count,
                update.seeded,
                &update.name,
            )
            .await
        {
            log_error!("[rss] persist runtime failed: {}", e);
        }
        self.apply_runtime_to_memory(source_id, update);
    }

    /// 抓取回流的运行态回写：**仅语义变化时落库**，纯时间戳前进只更新内存。
    ///
    /// NAS 空闲静默：定时拉取若没有新条目、错误/退避/名称/首轮标记都没变，
    /// 每次只会把 `last_fetch_at`/`last_success_at` 往前推——为此写一次
    /// SQLite（WAL 回写会唤醒休眠硬盘）毫无收益。内存镜像始终是最新值
    /// （due 判定与 UI「上次检查」都读它，见 [`RssManager::load`] 的合并），
    /// 下次有语义变化的落库会把累计的时间戳一并带上。
    ///
    /// 代价：进程重启后 DB 里的时间戳可能偏旧，该源会被判定为到期并立即多
    /// 拉一次，然后恢复正常节奏——可接受。
    ///
    /// 返回是否真的写了库。
    async fn record_runtime(&mut self, source_id: &str, update: RuntimeUpdate) -> bool {
        let needs_persist = self
            .source(source_id)
            .is_none_or(|old| runtime_needs_persist(old, &update));
        if needs_persist {
            self.persist_runtime(source_id, &update).await;
        } else {
            self.apply_runtime_to_memory(source_id, &update);
        }
        needs_persist
    }

    fn apply_runtime_to_memory(&mut self, source_id: &str, update: &RuntimeUpdate) {
        if let Some(s) = self.sources.iter_mut().find(|s| s.source_id == source_id) {
            s.last_fetch_at = update.last_fetch_at;
            s.last_success_at = update.last_success_at;
            s.last_error.clone_from(&update.last_error);
            s.fail_count = update.fail_count;
            s.seeded = update.seeded;
            s.name.clone_from(&update.name);
        }
    }
}

/// 一次抓取/重置后要回写的运行态列（对应 `rss_sources` 的运行列）。
struct RuntimeUpdate {
    last_fetch_at: i64,
    last_success_at: i64,
    last_error: String,
    fail_count: i32,
    seeded: bool,
    name: String,
}

/// 运行态回写是否有「语义变化」需要落库（纯函数）。
///
/// 不算语义变化的：`last_fetch_at`/`last_success_at` 前进（仅影响 due 判定与
/// UI 显示，内存里始终保持最新）；`fail_count` 增长但生效退避间隔不变（已
/// 封顶，或用户配置间隔本就高于封顶）——退避行为重启前后等价，只是重启后
/// UI 上的「连续失败 n 次」可能比内存里少。
/// 算变化的：错误文本、首轮标记、订阅名、生效退避间隔。
fn runtime_needs_persist(old: &RssSourceInfo, update: &RuntimeUpdate) -> bool {
    if old.last_error != update.last_error || old.seeded != update.seeded || old.name != update.name
    {
        return true;
    }
    let mut next = old.clone();
    next.fail_count = update.fail_count;
    effective_interval_secs(old) != effective_interval_secs(&next)
}

/// 把内存镜像里的运行态列覆盖到刚从 DB 装载的订阅上（按 `source_id` 匹配）。
fn merge_runtime_from_memory(fresh: &mut [RssSourceInfo], memory: &[RssSourceInfo]) {
    for s in fresh {
        if let Some(mem) = memory.iter().find(|m| m.source_id == s.source_id) {
            s.last_fetch_at = mem.last_fetch_at;
            s.last_success_at = mem.last_success_at;
            s.last_error.clone_from(&mem.last_error);
            s.fail_count = mem.fail_count;
            s.seeded = mem.seeded;
        }
    }
}

/// 编辑订阅后是否需要重置运行态。返回 `Some(seeded)` 表示要清零失败退避、
/// 清空错误并让下一轮 tick 立即重抓，`seeded` 为重置后的首轮标记：
/// url / provider 变化＝新 feed，其历史条目必须重新只播种不下载（`false`）；
/// 仅 Cookie / UA / 代理变化＝同一 feed，保留原标记。无关字段变化返回 `None`。
fn runtime_reset_on_update(old: &RssSourceInfo, new: &RssSourceInfo) -> Option<bool> {
    let feed_changed = old.url != new.url
        || old.provider_id != new.provider_id
        || old.provider_config != new.provider_config;
    let auth_changed = old.cookies != new.cookies
        || old.user_agent != new.user_agent
        || old.proxy_url != new.proxy_url;
    if feed_changed {
        Some(false)
    } else if auth_changed {
        Some(old.seeded)
    } else {
        None
    }
}

/// 到期判定（纯函数）。
///
/// 到期条件：启用 + 不在抓取中 + `now - last_fetch_at >= 生效间隔`。
/// 生效间隔见 [`effective_interval_secs`]。
///
/// 不依赖墙钟对齐——休眠唤醒后按「距上次抓取多久」判定，天然补触发。
pub fn due_sources<'a>(
    now: i64,
    sources: impl Iterator<Item = &'a RssSourceInfo>,
    in_flight: &HashSet<String>,
) -> Vec<String> {
    sources
        .filter(|s| s.enabled && !in_flight.contains(&s.source_id))
        .filter(|s| now.saturating_sub(s.last_fetch_at) >= effective_interval_secs(s))
        .map(|s| s.source_id.clone())
        .collect()
}

/// 生效抓取间隔（秒）= `interval_minutes × 2^fail_count`，封顶
/// [`MAX_BACKOFF_SECS`]，但**绝不短于**用户配置的间隔（配了 24h 的源不会因为
/// 封顶被拉快到 6h）。
///
/// 连续失败不自动停用订阅——私有 token feed 的抖动是常态，退避 + 侧边栏警告
/// 点已经够（§2.2）。
///
/// # Examples
///
/// ```
/// use fluxdown_engine::rss::effective_interval_secs;
/// use fluxdown_engine::rss::model::RssSourceInfo;
///
/// let healthy = RssSourceInfo { interval_minutes: 30, ..Default::default() };
/// assert_eq!(effective_interval_secs(&healthy), 1800);
///
/// let failing = RssSourceInfo { interval_minutes: 30, fail_count: 3, ..Default::default() };
/// assert_eq!(effective_interval_secs(&failing), 1800 * 8);
///
/// // 封顶 6h
/// let dead = RssSourceInfo { interval_minutes: 30, fail_count: 20, ..Default::default() };
/// assert_eq!(effective_interval_secs(&dead), 6 * 3600);
/// ```
#[must_use]
pub fn effective_interval_secs(source: &RssSourceInfo) -> i64 {
    let base = i64::from(source.interval_minutes.max(model::MIN_INTERVAL_MINUTES)) * 60;
    let shift = source.fail_count.clamp(0, 32) as u32;
    let scaled = base.checked_shl(shift).unwrap_or(i64::MAX);
    scaled.min(MAX_BACKOFF_SECS).max(base)
}

/// 当前 Unix 秒。
#[must_use]
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn rule_of(source: &RssSourceInfo) -> FilterRule {
    FilterRule {
        include: source.include_pattern.clone(),
        exclude: source.exclude_pattern.clone(),
        use_regex: source.use_regex,
        smart_episode: source.smart_episode,
        size_min_bytes: source.size_min_bytes,
        size_max_bytes: source.size_max_bytes,
    }
}

fn item_from_parsed(source_id: &str, parsed: &parser::ParsedItem, fetched_at: i64) -> RssItemInfo {
    RssItemInfo {
        source_id: source_id.to_string(),
        guid: parsed.guid.clone(),
        title: parsed.title.clone(),
        link: parsed.link.clone(),
        image_urls: Some(parsed.image_urls.clone()),
        enclosure_url: parsed.enclosure_url.clone(),
        enclosure_type: parsed.enclosure_type.clone(),
        resolver_item: parsed.resolver_item.clone(),
        enclosure_length: parsed.enclosure_length,
        pub_date: parsed.pub_date,
        fetched_at,
        status: RssItemStatus::New,
        task_id: String::new(),
        episode_key: String::new(),
        reason: String::new(),
    }
}

fn plan_for(source: &RssSourceInfo, item: &RssItemInfo) -> RssDownloadPlan {
    RssDownloadPlan {
        source_id: source.source_id.clone(),
        guid: item.guid.clone(),
        title: item.title.clone(),
        url: item.download_url().to_string(),
        resolver_item: item.resolver_item.clone(),
        save_dir: source.save_dir.clone(),
        queue_id: source.queue_id.clone(),
        start_paused: source.start_paused,
        cookies: source.cookies.clone(),
        user_agent: source.user_agent.clone(),
        proxy_url: source.proxy_url.clone(),
        referrer: if source.send_referer {
            feed_origin(&source.url)
        } else {
            String::new()
        },
        size_hint: item.enclosure_length,
        // 二段解析条目的下载地址是页面链接，enclosure 的类型与它无关。
        enclosure_type: if item.resolver_item.is_empty() {
            item.enclosure_type.clone()
        } else {
            String::new()
        },
        notify: source.notify_on_download,
    }
}

/// 一次抓取的出口决策（见 [`resolve_route`]）。
struct FeedRoute {
    proxy: ProxyConfig,
    /// 站点规则的逐请求路由器（重定向各跳按规则换出口）；无规则或订阅自带代理时为 `None`。
    egress: Option<Arc<EgressRouter>>,
    /// 订阅链接命中了站点规则（出口由规则决定，不做 Auto 失败转移）。
    rule_hit: bool,
}

/// 订阅出口：自带代理（`proxy_url` 非空）完全不看站点规则；否则订阅链接命中规则时取
/// 规则出口，未命中取全局解析结果。
fn resolve_route(proxy_url: &str, global: &ProxyConfig, url: &str, user_agent: &str) -> FeedRoute {
    if !proxy_url.is_empty() {
        return FeedRoute {
            proxy: ProxyConfig::from_proxy_url(proxy_url),
            egress: None,
            rule_hit: false,
        };
    }
    let Some(rules) = &global.rules else {
        return FeedRoute {
            proxy: global.resolve(),
            egress: None,
            rule_hit: false,
        };
    };
    match rules.first_match(url) {
        Some(rule) => FeedRoute {
            proxy: rule.egress().resolve(),
            egress: Some(Arc::new(EgressRouter::new(
                Arc::clone(rules),
                Some(rule.index()),
                user_agent,
                false,
            ))),
            rule_hit: true,
        },
        None => FeedRoute {
            proxy: global.resolve(),
            egress: Some(Arc::new(EgressRouter::new(
                Arc::clone(rules),
                None,
                user_agent,
                false,
            ))),
            rule_hit: false,
        },
    }
}

/// 非 coordinator 路径 Auto 模式抓取失败后的候选代理重试：先按 `attempt`
/// 用给定配置发起一次抓取；`eligible` 为 `true` 且直连失败时，依次改用
/// [`auto_proxy::resolve_candidates`] 给出的候选（手动字段优先于系统代理）
/// 各重试一次，命中即返回；全部候选也失败则返回最后一次错误。
/// `eligible = false` 时只跑一次给定配置，行为与不做失败转移一致。
async fn fetch_with_auto_failover<T, F, Fut>(
    direct_config: ProxyConfig,
    eligible: bool,
    global: &ProxyConfig,
    mut attempt: F,
) -> Result<T, String>
where
    F: FnMut(ProxyConfig) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let first_error = match attempt(direct_config).await {
        Ok(feed) => return Ok(feed),
        Err(e) => e,
    };
    if !eligible {
        return Err(first_error);
    }
    let mut last_error = first_error;
    let candidates = auto_proxy::resolve_candidates(global);
    let attempted = !candidates.is_empty();
    for candidate in candidates {
        tracing::debug!(
            proxy = candidate_label(candidate.source),
            "RSS Auto fetch retry"
        );
        match attempt(candidate.config).await {
            Ok(feed) => return Ok(feed),
            Err(e) => last_error = e,
        }
    }
    if attempted {
        last_error = format!("{last_error}（auto 候选代理均已重试）");
    }
    Err(last_error)
}

fn candidate_label(source: CandidateSource) -> &'static str {
    match source {
        CandidateSource::ManualFields => "手动",
        CandidateSource::System => "系统",
    }
}

/// feed 站点根地址，用作 `.torrent` 下载的 Referer（部分 PT 站校验来源）。
/// 解析失败时回退整条 feed 地址。
fn feed_origin(feed_url: &str) -> String {
    url::Url::parse(feed_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| format!("{}://{h}/", u.scheme())))
        .unwrap_or_else(|| feed_url.to_string())
}

type FetchRequest = SubscriptionFetchRequest;

struct BuiltinRssProvider;

impl SubscriptionProvider for BuiltinRssProvider {
    fn id(&self) -> &str {
        RSS_PROVIDER_ID
    }

    fn fetch(
        &self,
        request: SubscriptionFetchRequest,
    ) -> crate::subscription::SubscriptionFetchFuture {
        Box::pin(async move { fetch_feed(&request).await })
    }
}

/// 抓取并解析一个 feed。**只在 off-actor 任务里调用。**
async fn fetch_feed(req: &FetchRequest) -> Result<ParsedFeed, String> {
    let client = downloader::build_client(&req.proxy, &req.user_agent)
        .map_err(|e| format!("failed to build http client: {e}"))?;
    let spec = downloader::RequestSpec {
        cookies: req.cookies.clone(),
        egress: req.egress.clone(),
        ..downloader::RequestSpec::empty_get()
    };
    let mut resp = downloader::build_request(&client, &req.url, reqwest::Method::GET, &spec)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    // Enforce the limit while reading, including chunked and decoded responses.
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_FEED_BYTES as u64)
    {
        return Err("feed too large".to_string());
    }
    let base = resp.url().clone();
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if chunk.len() > MAX_FEED_BYTES - bytes.len() {
            return Err("feed too large".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    let permit = FEED_PARSERS.acquire().await.map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        parse_feed_at(&bytes, Some(&base))
    })
    .await
    .map_err(|e| format!("feed parser worker failed: {e}"))?
}

/// 抓取一个 `.torrent` 文件的原始字节。**只在 off-actor 任务里调用。**
///
/// 带上订阅级 Cookie 与 Referer——部分 PT 站的种子下载正是靠这两样鉴权
/// （`send_referer` 开关的实际用武之地）。
async fn fetch_torrent(req: &FetchRequest, referrer: &str) -> Result<Vec<u8>, String> {
    let client = downloader::build_client(&req.proxy, &req.user_agent)
        .map_err(|e| format!("failed to build http client: {e}"))?;
    let spec = downloader::RequestSpec {
        cookies: req.cookies.clone(),
        referrer: referrer.to_string(),
        egress: req.egress.clone(),
        ..downloader::RequestSpec::empty_get()
    };
    let resp = downloader::build_request(&client, &req.url, reqwest::Method::GET, &spec)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_TORRENT_BYTES as u64)
    {
        return Err("torrent file too large".to_string());
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > MAX_TORRENT_BYTES {
        return Err(format!("torrent file too large ({} bytes)", bytes.len()));
    }
    // 站点没登录时常常返回一个 200 的 HTML 登录页——不校验就会把网页存成
    // 种子再让 librqbit 报一句看不懂的解析错。bencode 字典必以 `d` 起头。
    if bytes.first() != Some(&b'd') {
        return Err("response is not a torrent file (login required?)".to_string());
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashSet;

    use super::{
        MAX_BACKOFF_SECS, RuntimeUpdate, due_sources, effective_interval_secs, feed_origin,
        fetch_with_auto_failover, plan_for, rule_of, runtime_needs_persist,
        runtime_reset_on_update, torrent_retry_delay_secs, url_looks_like_torrent,
    };
    use crate::proxy_config::{ProxyConfig, ProxyMode};
    use crate::rss::model::{RssItemInfo, RssItemStatus, RssSourceInfo};

    #[tokio::test]
    async fn fetch_rejects_oversized_chunked_feed_before_the_body_finishes() -> anyhow::Result<()> {
        use super::{FetchRequest, MAX_FEED_BYTES, fetch_feed};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let req = FetchRequest {
            url: format!("http://{}/feed", listener.local_addr()?),
            provider_id: super::RSS_PROVIDER_ID.into(),
            source_id: String::new(),
            provider_config: String::new(),
            cookies: String::new(),
            user_agent: String::new(),
            proxy_url: String::new(),
            proxy: ProxyConfig::default(),
            egress: None,
        };
        let (closed, keep_open) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            stream.read_exact(&mut [0; 4]).await?;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await?;
            stream
                .write_all(format!("{:x}\r\n", MAX_FEED_BYTES + 1).as_bytes())
                .await?;
            stream.write_all(&vec![b'x'; MAX_FEED_BYTES + 1]).await?;
            keep_open.await.map_err(std::io::Error::other)?;
            Ok::<(), std::io::Error>(())
        });
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(5), fetch_feed(&req)).await;
        let sent = closed.send(());
        server.await??;
        assert_eq!(sent, Ok(()));
        assert_eq!(result?.err().as_deref(), Some("feed too large"));
        Ok(())
    }

    fn mem_source(fail_count: i32, last_error: &str) -> RssSourceInfo {
        RssSourceInfo {
            source_id: "s1".to_string(),
            name: "feed".to_string(),
            interval_minutes: 30,
            seeded: true,
            fail_count,
            last_error: last_error.to_string(),
            last_fetch_at: 100,
            last_success_at: 100,
            ..Default::default()
        }
    }

    fn update_of(old: &RssSourceInfo) -> RuntimeUpdate {
        RuntimeUpdate {
            last_fetch_at: old.last_fetch_at + 1800,
            last_success_at: old.last_success_at + 1800,
            last_error: old.last_error.clone(),
            fail_count: old.fail_count,
            seeded: old.seeded,
            name: old.name.clone(),
        }
    }

    #[test]
    fn healthy_refetch_with_only_timestamps_advancing_is_not_persisted() {
        let old = mem_source(0, "");
        assert!(!runtime_needs_persist(&old, &update_of(&old)));
    }

    #[test]
    fn semantic_runtime_changes_are_persisted() {
        let healthy = mem_source(0, "");

        let mut u = update_of(&healthy);
        u.last_error = "HTTP 500".to_string();
        u.fail_count = 1;
        assert!(runtime_needs_persist(&healthy, &u), "first failure");

        let mut u = update_of(&healthy);
        u.seeded = false;
        assert!(runtime_needs_persist(&healthy, &u), "seeded flag flipped");

        let mut u = update_of(&healthy);
        u.name = "renamed by feed title".to_string();
        assert!(runtime_needs_persist(&healthy, &u), "name backfilled");

        let failing = mem_source(2, "HTTP 500");
        let mut u = update_of(&failing);
        u.last_error.clear();
        u.fail_count = 0;
        assert!(runtime_needs_persist(&failing, &u), "recovery clears error");

        let mut u = update_of(&failing);
        u.last_error = "timeout".to_string();
        assert!(runtime_needs_persist(&failing, &u), "different error text");

        let mut u = update_of(&failing);
        u.fail_count = 3;
        assert!(
            runtime_needs_persist(&failing, &u),
            "backoff interval still growing (2 -> 3 failures)"
        );
    }

    #[test]
    fn repeated_identical_failure_at_backoff_cap_is_not_persisted() {
        // 30min × 2^4 = 8h 已越过 6h 封顶：再失败只是计数增长，退避间隔不变。
        let capped = mem_source(4, "HTTP 500");
        let mut u = update_of(&capped);
        u.fail_count = 5;
        assert!(!runtime_needs_persist(&capped, &u));

        // 用户配置的间隔本就高于封顶（24h）：任何失败计数增长都不改变间隔。
        let mut slow = mem_source(1, "HTTP 500");
        slow.interval_minutes = 24 * 60;
        let mut u = update_of(&slow);
        u.fail_count = 2;
        assert!(!runtime_needs_persist(&slow, &u));
    }

    #[test]
    fn reload_keeps_in_memory_runtime_but_takes_user_fields_from_db() {
        // DB 里的运行态偏旧（无语义变化的抓取没落库），用户字段已被编辑。
        let mut db_row = mem_source(0, "");
        db_row.last_fetch_at = 10;
        db_row.last_success_at = 10;
        db_row.name = "edited name".to_string();
        db_row.unread_count = 7;
        let mut fresh = vec![db_row, source("other", 30, 5)];

        let mut mem = mem_source(3, "HTTP 500");
        mem.last_fetch_at = 9_999;
        mem.last_success_at = 8_888;
        super::merge_runtime_from_memory(&mut fresh, &[mem]);

        assert_eq!(fresh[0].last_fetch_at, 9_999);
        assert_eq!(fresh[0].last_success_at, 8_888);
        assert_eq!(fresh[0].fail_count, 3);
        assert_eq!(fresh[0].last_error, "HTTP 500");
        assert_eq!(fresh[0].name, "edited name");
        assert_eq!(fresh[0].unread_count, 7);
        // 内存里没有的订阅（新建）保持 DB 值。
        assert_eq!(fresh[1].last_fetch_at, 5);
    }

    fn source(id: &str, interval: i32, last_fetch: i64) -> RssSourceInfo {
        RssSourceInfo {
            source_id: id.to_string(),
            url: format!("https://feed.test/{id}"),
            interval_minutes: interval,
            last_fetch_at: last_fetch,
            ..Default::default()
        }
    }

    #[test]
    fn brand_new_source_is_due_immediately() {
        let sources = [source("s1", 30, 0)];
        let due = due_sources(1_000_000, sources.iter(), &HashSet::new());
        assert_eq!(due, vec!["s1".to_string()]);
    }

    #[test]
    fn source_is_not_due_before_its_interval_elapses() {
        let now = 1_000_000;
        let sources = [source("s1", 30, now - 1799)];
        assert!(due_sources(now, sources.iter(), &HashSet::new()).is_empty());
        let sources = [source("s1", 30, now - 1800)];
        assert_eq!(due_sources(now, sources.iter(), &HashSet::new()).len(), 1);
    }

    #[test]
    fn disabled_and_in_flight_sources_are_skipped() {
        let now = 1_000_000;
        let mut disabled = source("s1", 30, 0);
        disabled.enabled = false;
        assert!(due_sources(now, [disabled].iter(), &HashSet::new()).is_empty());

        let busy = source("s2", 30, 0);
        let in_flight: HashSet<String> = ["s2".to_string()].into_iter().collect();
        assert!(
            due_sources(now, [busy].iter(), &in_flight).is_empty(),
            "a source already being fetched must not be dispatched again"
        );
    }

    #[test]
    fn backoff_doubles_per_failure_and_caps_at_six_hours() {
        let mut s = source("s1", 30, 0);
        assert_eq!(effective_interval_secs(&s), 1800);
        s.fail_count = 1;
        assert_eq!(effective_interval_secs(&s), 3600);
        s.fail_count = 3;
        assert_eq!(effective_interval_secs(&s), 1800 * 8);
        // 第 4 次失败起 30min×16 = 8h 已越过 6h 封顶
        s.fail_count = 4;
        assert_eq!(effective_interval_secs(&s), MAX_BACKOFF_SECS);
        s.fail_count = 10;
        assert_eq!(effective_interval_secs(&s), MAX_BACKOFF_SECS);
        // 极端 fail_count 不能溢出成负数/零间隔（那会变成每 tick 猛打站点）
        s.fail_count = i32::MAX;
        assert_eq!(effective_interval_secs(&s), MAX_BACKOFF_SECS);
    }

    #[test]
    fn backoff_never_shortens_a_long_user_interval() {
        let mut s = source("s1", 24 * 60, 0);
        s.fail_count = 8;
        assert_eq!(
            effective_interval_secs(&s),
            24 * 3600,
            "capping at 6h must not speed up a 24h subscription"
        );
    }

    #[test]
    fn zero_interval_is_clamped_instead_of_spinning() {
        let s = source("s1", 0, 0);
        assert_eq!(effective_interval_secs(&s), 60);
    }

    #[test]
    fn feed_origin_reduces_to_site_root() {
        assert_eq!(
            feed_origin("https://mikanani.me/RSS/MyBangumi?token=abc"),
            "https://mikanani.me/"
        );
        assert_eq!(feed_origin("not a url"), "not a url");
    }

    #[test]
    fn plan_prefers_enclosure_and_honours_referer_switch() {
        let mut s = source("s1", 30, 0);
        s.url = "https://mikanani.me/RSS/MyBangumi?token=abc".to_string();
        s.queue_id = "anime".to_string();
        s.save_dir = "D:/Anime".to_string();
        let item = RssItemInfo {
            guid: "g1".to_string(),
            title: "[ANi] Show - 02".to_string(),
            link: "https://mikanani.me/Home/Episode/g1".to_string(),
            enclosure_url: "https://mikanani.me/Download/g1.torrent".to_string(),
            enclosure_length: 418 * 1024 * 1024,
            status: RssItemStatus::New,
            ..Default::default()
        };
        let plan = plan_for(&s, &item);
        assert_eq!(plan.url, "https://mikanani.me/Download/g1.torrent");
        assert_eq!(plan.referrer, "https://mikanani.me/");
        assert_eq!(plan.queue_id, "anime");
        assert_eq!(plan.size_hint, 418 * 1024 * 1024);

        let mut resolver_item = item.clone();
        resolver_item.resolver_item = "ep:123@q:80".to_string();
        let resolver_plan = plan_for(&s, &resolver_item);
        assert_eq!(resolver_plan.url, resolver_item.link);
        assert_eq!(resolver_plan.resolver_item, "ep:123@q:80");

        s.send_referer = false;
        assert!(plan_for(&s, &item).referrer.is_empty());

        // 无 enclosure 时回退条目链接
        let no_enclosure = RssItemInfo {
            enclosure_url: String::new(),
            ..item
        };
        assert_eq!(
            plan_for(&s, &no_enclosure).url,
            "https://mikanani.me/Home/Episode/g1"
        );
    }

    #[test]
    fn rule_projection_carries_every_filter_field() {
        let s = RssSourceInfo {
            include_pattern: "1080P".to_string(),
            exclude_pattern: "720P".to_string(),
            use_regex: true,
            smart_episode: true,
            size_min_bytes: 100,
            size_max_bytes: 200,
            ..Default::default()
        };
        let rule = rule_of(&s);
        assert_eq!(rule.include, "1080P");
        assert_eq!(rule.exclude, "720P");
        assert!(rule.use_regex);
        assert!(rule.smart_episode);
        assert_eq!(rule.size_min_bytes, 100);
        assert_eq!(rule.size_max_bytes, 200);
    }

    // -----------------------------------------------------------------------
    // 状态机（§2.2 行为语义表）——用内存 SQLite 跑真实落库路径
    // -----------------------------------------------------------------------

    use std::sync::Arc;

    use super::{RssFetchOutcome, RssManager};
    use crate::db::Db;
    use crate::rss::parser::{ParsedFeed, ParsedItem};

    async fn manager() -> RssManager {
        let db = Db::connect("sqlite::memory:").await.expect("open mem db");
        RssManager::new(db, Arc::new(crate::NoopSink))
    }

    fn parsed(guid: &str, title: &str, size: i64, pub_date: i64) -> ParsedItem {
        ParsedItem {
            guid: guid.to_string(),
            title: title.to_string(),
            link: format!("https://feed.test/item/{guid}"),
            enclosure_url: format!("https://feed.test/dl/{guid}.torrent"),
            enclosure_type: String::new(),
            resolver_item: String::new(),
            enclosure_length: size,
            pub_date,
            image_urls: Vec::new(),
        }
    }

    fn feed(items: Vec<ParsedItem>) -> ParsedFeed {
        ParsedFeed {
            title: "Test Feed".to_string(),
            link: "https://feed.test/".to_string(),
            items,
        }
    }

    async fn fetched(m: &mut RssManager, id: &str, items: Vec<ParsedItem>) -> Vec<String> {
        let plans = m
            .apply_fetch(RssFetchOutcome {
                source_id: id.to_string(),
                feed: feed(items),
                error: String::new(),
            })
            .await;
        // 模拟 DownloadManager 建任务成功后的回写，否则下一轮会重复派发。
        for p in &plans {
            m.mark_downloaded(&p.source_id, &p.guid, &format!("task-{}", p.guid))
                .await;
        }
        plans.into_iter().map(|p| p.guid).collect()
    }

    async fn subscribe(m: &mut RssManager, source: RssSourceInfo) -> String {
        m.create_source(source).await.expect("create source")
    }

    #[tokio::test]
    async fn first_round_marks_history_read_and_downloads_nothing() {
        let mut m = manager().await;
        let id = subscribe(
            &mut m,
            RssSourceInfo {
                url: "https://feed.test/rss".to_string(),
                ..Default::default()
            },
        )
        .await;

        let downloaded = fetched(
            &mut m,
            &id,
            vec![
                parsed("a", "[X] Show - 01", 100, 10),
                parsed("b", "[X] Show - 02", 100, 20),
                parsed("c", "[X] Show - 03", 100, 30),
            ],
        )
        .await;
        assert!(
            downloaded.is_empty(),
            "first round must never auto-download history"
        );

        let items = m.db.load_rss_items(&id, 100).await.expect("items");
        assert_eq!(items.len(), 3);
        assert!(
            items.iter().all(|i| i.status == RssItemStatus::SeedSkipped),
            "every history item is marked read, not downloaded"
        );
        assert!(m.source(&id).expect("source").seeded);
    }

    #[tokio::test]
    async fn second_round_downloads_only_genuinely_new_items() {
        let mut m = manager().await;
        let id = subscribe(
            &mut m,
            RssSourceInfo {
                url: "https://feed.test/rss".to_string(),
                ..Default::default()
            },
        )
        .await;
        fetched(&mut m, &id, vec![parsed("a", "[X] Show - 01", 100, 10)]).await;

        // 第二轮：feed 重发 a（同 guid）+ 新增 b
        let downloaded = fetched(
            &mut m,
            &id,
            vec![
                parsed("a", "[X] Show - 01 (edited title)", 100, 10),
                parsed("b", "[X] Show - 02", 100, 20),
            ],
        )
        .await;
        assert_eq!(downloaded, vec!["b".to_string()]);

        // 第三轮：完全没有新条目 → 不再派发任何东西
        assert!(
            fetched(&mut m, &id, vec![parsed("b", "[X] Show - 02", 100, 20)])
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn deleting_download_task_prevents_refetch_dispatch_but_allows_manual_redownload() {
        let mut m = manager().await;
        let id = subscribe(
            &mut m,
            RssSourceInfo {
                url: "https://feed.test/rss".to_string(),
                ..Default::default()
            },
        )
        .await;
        fetched(&mut m, &id, Vec::new()).await;
        assert_eq!(
            fetched(&mut m, &id, vec![parsed("episode", "Episode", 100, 10)]).await,
            vec!["episode"]
        );
        m.db.insert_task(
            "task-episode",
            "https://feed.test/dl/episode",
            "episode",
            "/tmp",
            3,
            0,
            "",
            "",
            "",
            0,
        )
        .await
        .expect("persist download task");
        assert_eq!(
            m.db.delete_task("task-episode").await.expect("delete"),
            vec![id.clone()]
        );
        let old =
            m.db.rss_item(&id, "episode")
                .await
                .expect("query")
                .expect("item");
        assert_eq!(old.status, RssItemStatus::Ignored);
        assert!(old.task_id.is_empty());
        assert_eq!(
            fetched(
                &mut m,
                &id,
                vec![
                    parsed("episode", "Episode", 100, 10),
                    parsed("next", "Next", 100, 20)
                ]
            )
            .await,
            vec!["next"],
            "existing episode stays read while a genuinely new item still dispatches"
        );
        assert!(m.manual_download(&id, "episode").await.is_some());
    }

    #[tokio::test]
    async fn per_round_cap_defers_the_remainder_oldest_first() {
        let mut m = manager().await;
        let id = subscribe(
            &mut m,
            RssSourceInfo {
                url: "https://feed.test/rss".to_string(),
                max_per_fetch: 2,
                ..Default::default()
            },
        )
        .await;
        fetched(&mut m, &id, Vec::new()).await; // 首轮播种（空 feed）

        let batch: Vec<ParsedItem> = (1..=5)
            .map(|n| parsed(&format!("g{n}"), &format!("[X] Show - {n:02}"), 100, n))
            .collect();
        let round1 = fetched(&mut m, &id, batch.clone()).await;
        assert_eq!(
            round1,
            vec!["g1".to_string(), "g2".to_string()],
            "cap honoured, oldest first"
        );

        // 下一轮即便 feed 没变，积压的 g3/g4 继续派发（不丢、不插队）
        let round2 = fetched(&mut m, &id, batch.clone()).await;
        assert_eq!(round2, vec!["g3".to_string(), "g4".to_string()]);
        let round3 = fetched(&mut m, &id, batch).await;
        assert_eq!(round3, vec!["g5".to_string()]);
    }

    #[tokio::test]
    async fn collect_mode_records_items_but_never_creates_tasks() {
        let mut m = manager().await;
        let id = subscribe(
            &mut m,
            RssSourceInfo {
                url: "https://feed.test/rss".to_string(),
                auto_download: false,
                ..Default::default()
            },
        )
        .await;
        fetched(&mut m, &id, Vec::new()).await;
        let downloaded = fetched(&mut m, &id, vec![parsed("a", "[X] Show - 01", 100, 10)]).await;
        assert!(downloaded.is_empty());

        let items = m.db.load_rss_items(&id, 100).await.expect("items");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].status,
            RssItemStatus::New,
            "collect mode leaves items pickable by hand"
        );
    }

    #[tokio::test]
    async fn rules_and_smart_dedup_are_persisted_with_their_reason_codes() {
        let mut m = manager().await;
        let id = subscribe(
            &mut m,
            RssSourceInfo {
                url: "https://feed.test/rss".to_string(),
                exclude_pattern: "720P".to_string(),
                smart_episode: true,
                ..Default::default()
            },
        )
        .await;
        fetched(&mut m, &id, Vec::new()).await;

        let downloaded = fetched(
            &mut m,
            &id,
            vec![
                parsed("ani", "[ANi] Show - 02 [1080P]", 100, 30),
                parsed("sakura", "[Sakurato] Show - 02 [1080P]", 100, 20),
                parsed("low", "[Other] Show - 03 [720P]", 100, 10),
            ],
        )
        .await;
        assert_eq!(downloaded, vec!["ani".to_string()]);

        let items = m.db.load_rss_items(&id, 100).await.expect("items");
        let by_guid = |g: &str| {
            items
                .iter()
                .find(|i| i.guid == g)
                .cloned()
                .expect("item present")
        };
        assert_eq!(by_guid("ani").status, RssItemStatus::Downloaded);
        assert_eq!(by_guid("ani").task_id, "task-ani");
        assert_eq!(by_guid("sakura").status, RssItemStatus::DuplicateEpisode);
        assert_eq!(by_guid("sakura").reason, "dup_episode");
        assert_eq!(by_guid("low").status, RssItemStatus::Filtered);
        assert_eq!(by_guid("low").reason, "excluded");
    }

    #[tokio::test]
    async fn manual_download_works_for_every_item_state_including_downloaded() {
        let mut m = manager().await;
        let id = subscribe(
            &mut m,
            RssSourceInfo {
                url: "https://feed.test/rss".to_string(),
                exclude_pattern: "720P".to_string(),
                ..Default::default()
            },
        )
        .await;
        fetched(&mut m, &id, Vec::new()).await;
        fetched(
            &mut m,
            &id,
            vec![parsed("low", "[X] Show - 01 [720P]", 100, 10)],
        )
        .await;

        // 被规则过滤掉的条目仍可手动「仍要下载」。
        let plan = m
            .manual_download(&id, "low")
            .await
            .expect("filtered items stay manually downloadable");
        assert_eq!(plan.url, "https://feed.test/dl/low.torrent");

        // 已下载的条目**也**要能重下：任务可能被删了、下崩了，或者只是想
        // 再来一遍。挡住重下没有任何好处，只会逼用户去别处找种子。
        m.mark_downloaded(&id, "low", "task-low").await;
        assert!(
            m.manual_download(&id, "low").await.is_some(),
            "an already-downloaded item must remain re-downloadable"
        );

        // 不存在的条目仍然返回 None。
        assert!(m.manual_download(&id, "nope").await.is_none());
    }

    #[tokio::test]
    async fn fetch_failure_records_the_error_and_backs_off() {
        let mut m = manager().await;
        let id = subscribe(
            &mut m,
            RssSourceInfo {
                url: "https://feed.test/rss".to_string(),
                ..Default::default()
            },
        )
        .await;
        for _ in 0..2 {
            let plans = m
                .apply_fetch(RssFetchOutcome {
                    source_id: id.clone(),
                    feed: ParsedFeed::default(),
                    error: "HTTP 403 Forbidden".to_string(),
                })
                .await;
            assert!(plans.is_empty());
        }
        let s = m.source(&id).expect("source").clone();
        assert_eq!(s.fail_count, 2);
        assert_eq!(s.last_error, "HTTP 403 Forbidden");
        assert!(
            s.enabled,
            "repeated failures must not auto-disable the feed"
        );
        assert!(!s.seeded, "a failed first round must stay unseeded");
        assert_eq!(effective_interval_secs(&s), 30 * 60 * 4);
    }

    #[tokio::test]
    async fn deleting_a_source_cascades_items_but_keeps_task_links_intact() {
        let mut m = manager().await;
        let id = subscribe(
            &mut m,
            RssSourceInfo {
                url: "https://feed.test/rss".to_string(),
                ..Default::default()
            },
        )
        .await;
        fetched(&mut m, &id, vec![parsed("a", "[X] Show - 01", 100, 10)]).await;
        assert!(m.delete_source(&id).await);
        assert!(m.source(&id).is_none());
        assert!(
            m.db.load_rss_items(&id, 100)
                .await
                .expect("items")
                .is_empty()
        );
        assert!(!m.delete_source(&id).await, "deleting twice is a no-op");
    }

    #[tokio::test]
    async fn results_for_a_source_deleted_mid_fetch_are_dropped() {
        let mut m = manager().await;
        let plans = m
            .apply_fetch(RssFetchOutcome {
                source_id: "ghost".to_string(),
                feed: feed(vec![parsed("a", "[X] Show - 01", 100, 10)]),
                error: String::new(),
            })
            .await;
        assert!(plans.is_empty(), "no source, no tasks, no orphan rows");
    }

    fn auto_proxy_global() -> ProxyConfig {
        ProxyConfig {
            mode: ProxyMode::Auto,
            host: "10.0.0.1".to_string(),
            port: 1080,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn auto_failover_skips_retry_when_not_eligible() {
        let global = auto_proxy_global();
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let attempts_clone = attempts.clone();
        let result = fetch_with_auto_failover::<ParsedFeed, _, _>(
            ProxyConfig::default(),
            false, // 订阅有专属代理，或全局非 Auto：不重试
            &global,
            move |_cfg| {
                let attempts = attempts_clone.clone();
                async move {
                    *attempts.lock().expect("lock") += 1;
                    Err("direct failed".to_string())
                }
            },
        )
        .await;
        assert_eq!(result, Err("direct failed".to_string()));
        assert_eq!(
            *attempts.lock().expect("lock"),
            1,
            "not eligible must not retry"
        );
    }

    #[tokio::test]
    async fn auto_failover_retries_manual_candidate_after_direct_failure() {
        let global = auto_proxy_global();
        let modes_tried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let modes_clone = modes_tried.clone();
        let result = fetch_with_auto_failover(ProxyConfig::default(), true, &global, move |cfg| {
            let modes = modes_clone.clone();
            async move {
                modes.lock().expect("lock").push(cfg.mode.clone());
                if cfg.mode == ProxyMode::Manual {
                    Ok(ParsedFeed::default())
                } else {
                    Err("direct failed".to_string())
                }
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "must succeed once the manual candidate is tried"
        );
        assert_eq!(
            *modes_tried.lock().expect("lock"),
            vec![ProxyMode::None, ProxyMode::Manual],
            "direct attempted first, then the manual candidate"
        );
    }

    #[tokio::test]
    async fn auto_failover_exhausts_all_candidates_and_returns_last_error() {
        let global = auto_proxy_global();
        let attempts = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let attempts_clone = attempts.clone();
        let result = fetch_with_auto_failover::<ParsedFeed, _, _>(
            ProxyConfig::default(),
            true,
            &global,
            move |_cfg| {
                let attempts = attempts_clone.clone();
                async move {
                    *attempts.lock().expect("lock") += 1;
                    Err("failed".to_string())
                }
            },
        )
        .await;
        assert_eq!(result, Err("failed（auto 候选代理均已重试）".to_string()));
        // 直连 + 手动候选至少 2 次；测试机若配置了系统代理会再多一次系统候选。
        assert!(*attempts.lock().expect("lock") >= 2);
    }

    #[test]
    fn torrent_url_detection_covers_extensionless_private_tracker_links() {
        assert!(url_looks_like_torrent(
            "https://pt.example/download.php?id=1&passkey=abc"
        ));
        assert!(url_looks_like_torrent(
            "https://jackett.local/dl/indexer/?jackett_apikey=k&path=Zm9v&file=x"
        ));
        assert!(url_looks_like_torrent(
            "https://mikanani.me/Download/a.torrent"
        ));
        // 无 passkey 的 download.php 可能是任意文件，不能误判为种子。
        assert!(!url_looks_like_torrent(
            "https://site.example/download.php?id=1"
        ));
        assert!(!url_looks_like_torrent("magnet:?xt=urn:btih:deadbeef"));
        assert!(!url_looks_like_torrent("https://cdn.example/ep01.mp4"));
    }

    /// feed 声明 `application/x-bittorrent` 即视为种子（无扩展名的 PT 链接也是），
    /// 二段解析条目的下载地址是页面链接，不受 enclosure 类型影响。
    #[test]
    fn declared_torrent_mime_marks_plan_as_torrent() {
        let s = RssSourceInfo::default();
        let item = RssItemInfo {
            guid: "g".to_string(),
            enclosure_url: "https://pt.example/dl?id=7".to_string(),
            enclosure_type: "application/x-bittorrent".to_string(),
            ..Default::default()
        };
        assert!(plan_for(&s, &item).is_torrent_file());

        let untyped = RssItemInfo {
            enclosure_type: String::new(),
            ..item.clone()
        };
        assert!(!plan_for(&s, &untyped).is_torrent_file());

        let resolver = RssItemInfo {
            link: "https://site.example/page/7".to_string(),
            resolver_item: "ep:7".to_string(),
            ..item
        };
        assert!(!plan_for(&s, &resolver).is_torrent_file());
    }

    #[test]
    fn torrent_retry_delay_doubles_and_is_capped() {
        assert_eq!(torrent_retry_delay_secs(1), 600);
        assert_eq!(torrent_retry_delay_secs(2), 1200);
        assert_eq!(torrent_retry_delay_secs(3), 2400);
        assert_eq!(torrent_retry_delay_secs(40), 24 * 60 * 60);
    }

    #[test]
    fn update_resets_runtime_only_when_feed_or_auth_changes() {
        let old = RssSourceInfo {
            url: "https://a.test/rss".to_string(),
            seeded: true,
            fail_count: 5,
            ..Default::default()
        };
        let same = old.clone();
        assert_eq!(runtime_reset_on_update(&old, &same), None);

        let mut renamed = old.clone();
        renamed.name = "n".to_string();
        assert_eq!(runtime_reset_on_update(&old, &renamed), None);

        let mut new_cookie = old.clone();
        new_cookie.cookies = "uid=1".to_string();
        assert_eq!(runtime_reset_on_update(&old, &new_cookie), Some(true));

        let mut new_url = old.clone();
        new_url.url = "https://b.test/rss".to_string();
        assert_eq!(runtime_reset_on_update(&old, &new_url), Some(false));
    }

    /// 记录收到的请求并返回固定 feed 的插件 provider 替身。
    struct RecordingProvider {
        seen: std::sync::Mutex<Vec<crate::subscription::SubscriptionFetchRequest>>,
    }

    impl crate::subscription::SubscriptionProvider for RecordingProvider {
        fn id(&self) -> &str {
            "*"
        }

        fn fetch(
            &self,
            request: crate::subscription::SubscriptionFetchRequest,
        ) -> crate::subscription::SubscriptionFetchFuture {
            self.seen.lock().expect("recording lock").push(request);
            Box::pin(async move { Ok(feed(vec![parsed("g1", "Episode 1", 0, 1)])) })
        }
    }

    #[tokio::test]
    async fn validate_routes_plugin_provider_with_its_config() {
        let mut m = manager().await;
        let provider = std::sync::Arc::new(RecordingProvider {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        m.set_fallback_provider(provider.clone());
        let outcome = m
            .validate_future(
                "req".to_string(),
                " demo-json ".to_string(),
                " {\"quality\":\"hd\"} ".to_string(),
                "https://feed.test/list".to_string(),
                String::new(),
                String::new(),
                String::new(),
                &ProxyConfig::default(),
                "UA/1",
            )
            .await;
        assert_eq!(outcome.error, "");
        assert_eq!(outcome.feed_title, "Test Feed");
        assert_eq!(outcome.items.len(), 1);
        let seen = provider.seen.lock().expect("recording lock");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].provider_id, "demo-json");
        assert_eq!(seen[0].provider_config, "{\"quality\":\"hd\"}");
        assert_eq!(seen[0].url, "https://feed.test/list");
        assert_eq!(seen[0].user_agent, "UA/1");
    }

    #[tokio::test]
    async fn validate_unknown_provider_without_router_reports_not_installed() {
        let m = manager().await;
        let outcome = m
            .validate_future(
                String::new(),
                "ghost".to_string(),
                String::new(),
                "https://feed.test/list".to_string(),
                String::new(),
                String::new(),
                String::new(),
                &ProxyConfig::default(),
                "UA/1",
            )
            .await;
        assert_eq!(outcome.error, "subscription provider not installed: ghost");
        assert!(outcome.items.is_empty());
    }
}
