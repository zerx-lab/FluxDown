//! RSS 订阅的引擎侧领域类型。
//!
//! 与 [`crate::model`] 同惯例：纯数据、不绑定序列化协议；
//! 宿主各自用 `From` 转换成自己的 wire DTO。

/// 条目在订阅流中的状态。
///
/// wire 表示为 `i32`（与任务状态码同惯例，见 `.omp/knowledge/engine.md`），转换经
/// [`RssItemStatus::as_i32`] / [`RssItemStatus::from_i32`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RssItemStatus {
    /// 新条目：尚未下载也未被处置（侧边栏 badge 计数的唯一来源）。
    #[default]
    New,
    /// 已创建下载任务（`task_id` 回链）。
    Downloaded,
    /// 用户手动忽略 / 标记已读。
    Ignored,
    /// 未通过订阅的过滤规则（具体原因见 `reason`）。
    Filtered,
    /// 智能剧集去重判定为重复（同源同集已下过）。
    DuplicateEpisode,
    /// 首轮抓取的历史条目：只标记不下载（防「一订阅灌 200 个历史种子」）。
    SeedSkipped,
}

impl RssItemStatus {
    /// wire 编码。
    #[must_use]
    pub fn as_i32(self) -> i32 {
        match self {
            Self::New => 0,
            Self::Downloaded => 1,
            Self::Ignored => 2,
            Self::Filtered => 3,
            Self::DuplicateEpisode => 4,
            Self::SeedSkipped => 5,
        }
    }

    /// wire 解码；未知码回退 [`RssItemStatus::New`]（旧库/新客户端互操作）。
    #[must_use]
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Downloaded,
            2 => Self::Ignored,
            3 => Self::Filtered,
            4 => Self::DuplicateEpisode,
            5 => Self::SeedSkipped,
            _ => Self::New,
        }
    }
}

/// 一个订阅源的完整配置与运行态（兼容保留 `Rss` 命名）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RssSourceInfo {
    /// UUID 主键。
    pub source_id: String,
    /// 负责获取订阅内容的 provider。空值在归一化时回退为 [`RSS_PROVIDER_ID`]。
    pub provider_id: String,
    /// provider 专属配置（JSON 字符串；空值表示无额外配置）。
    pub provider_config: String,
    /// feed 地址（可含 token 的私有 feed）。
    pub url: String,
    /// 显示名（空 = 用 feed 标题；创建时由验证结果回填）。
    pub name: String,
    /// 停用 = 保留配置与历史但不再抓取（区别于删除，qB#23941）。
    pub enabled: bool,
    /// false = 收集模式：只收集条目供手动挑选，不自动建任务。
    pub auto_download: bool,
    /// 自动创建的任务是否以 paused 落库（「稍后下载」语义）。
    pub start_paused: bool,
    /// 目标队列（空 = 内置主队列）。
    pub queue_id: String,
    /// 保存目录（空 = 队列目录 → 全局默认目录）。
    pub save_dir: String,
    /// 抓取间隔（分钟，最小 1）。
    pub interval_minutes: i32,
    /// 包含关键词（`|` = 或，空格 = 且；空 = 不过滤）。
    pub include_pattern: String,
    /// 排除关键词（同上语法；空 = 不排除）。
    pub exclude_pattern: String,
    /// 包含/排除按正则解析。
    pub use_regex: bool,
    /// 智能剧集去重。
    pub smart_episode: bool,
    /// 体积下限（字节，0 = 不限）。
    pub size_min_bytes: i64,
    /// 体积上限（字节，0 = 不限）。
    pub size_max_bytes: i64,
    /// 下载 `.torrent` 时携带 feed 站点 Referer（部分 PT 站校验来源）。
    pub send_referer: bool,
    /// 自动创建任务时发送通知（AutoBangumi #64）。
    pub notify_on_download: bool,
    /// 每轮最多新建任务数（1..=100）。
    pub max_per_fetch: i32,
    /// 请求 Cookie（空 = 无）。
    pub cookies: String,
    /// 独立 User-Agent（空 = 全局）。
    pub user_agent: String,
    /// 独立代理（空 = 全局）。
    pub proxy_url: String,
    /// 上次发起抓取的 Unix 秒（0 = 从未）。due 判定的基准。
    pub last_fetch_at: i64,
    /// 上次抓取成功的 Unix 秒（0 = 从未）。
    pub last_success_at: i64,
    /// 上次失败原因（空 = 健康）。
    pub last_error: String,
    /// 连续失败次数（指数退避的指数；成功清零）。
    pub fail_count: i32,
    /// 首轮抓取是否已完成（false = 下一轮的条目全部标记
    /// [`RssItemStatus::SeedSkipped`] 而不下载）。
    pub seeded: bool,
    /// 侧边栏排序位（越小越靠前）。
    pub position: i32,
    /// **派生字段**（不落库）：该源 `status = New` 的条目数，供侧边栏 badge。
    /// 由 [`crate::db::Db::load_all_rss_sources`] 的子查询一次算出——它变化
    /// 比配置频繁得多，但和配置同批推送才不会让 UI 出现「列表已到、计数还
    /// 没到」的两段式闪烁。
    pub unread_count: i32,
}

impl Default for RssSourceInfo {
    fn default() -> Self {
        Self {
            source_id: String::new(),
            provider_id: RSS_PROVIDER_ID.to_string(),
            provider_config: String::new(),
            url: String::new(),
            name: String::new(),
            enabled: true,
            auto_download: true,
            start_paused: false,
            queue_id: String::new(),
            save_dir: String::new(),
            interval_minutes: DEFAULT_INTERVAL_MINUTES,
            include_pattern: String::new(),
            exclude_pattern: String::new(),
            use_regex: false,
            smart_episode: false,
            size_min_bytes: 0,
            size_max_bytes: 0,
            send_referer: true,
            notify_on_download: true,
            max_per_fetch: DEFAULT_MAX_PER_FETCH,
            cookies: String::new(),
            user_agent: String::new(),
            proxy_url: String::new(),
            last_fetch_at: 0,
            last_success_at: 0,
            last_error: String::new(),
            fail_count: 0,
            seeded: false,
            position: 0,
            unread_count: 0,
        }
    }
}

/// 内置 RSS/Atom/JSON Feed provider 的稳定 ID。
pub const RSS_PROVIDER_ID: &str = "rss";

/// 默认抓取间隔（分钟）。
pub const DEFAULT_INTERVAL_MINUTES: i32 = 30;
/// 默认单轮新建任务上限。
pub const DEFAULT_MAX_PER_FETCH: i32 = 20;
/// 单轮新建任务上限的合法区间。
pub const MAX_PER_FETCH_RANGE: (i32, i32) = (1, 100);
/// 抓取间隔的合法下限（分钟）——防误配成 0 导致 tick 空转打站点。
pub const MIN_INTERVAL_MINUTES: i32 = 1;

impl RssSourceInfo {
    /// 归一化用户可写字段到合法区间。CRUD 写入前统一调用，让 REST / 信号 /
    /// CLI 三条入口共享同一套边界，而不是各自校验。
    pub fn normalize(&mut self) {
        if self.provider_id.trim().is_empty() {
            self.provider_id = RSS_PROVIDER_ID.to_string();
        } else {
            self.provider_id = self.provider_id.trim().to_string();
        }
        self.provider_config = self.provider_config.trim().to_string();
        self.interval_minutes = self.interval_minutes.max(MIN_INTERVAL_MINUTES);
        self.max_per_fetch = self
            .max_per_fetch
            .clamp(MAX_PER_FETCH_RANGE.0, MAX_PER_FETCH_RANGE.1);
        self.size_min_bytes = self.size_min_bytes.max(0);
        self.size_max_bytes = self.size_max_bytes.max(0);
        self.url = self.url.trim().to_string();
        self.name = self.name.trim().to_string();
    }

    /// 展示名：`name` 为空时回退到 feed 地址（宿主 UI 不必各自兜底）。
    #[must_use]
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.url
        } else {
            &self.name
        }
    }
}

/// 订阅流中的一个条目（`rss_items` 表的一行）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RssItemInfo {
    /// 所属订阅（预览态为空）。
    pub source_id: String,
    /// 去重主键（见 [`crate::rss::parser`] 的回退链）。
    pub guid: String,
    /// 条目标题。
    pub title: String,
    /// 条目页面链接。
    pub link: String,
    /// Description image URLs; absent for older rows that have not been backfilled.
    pub image_urls: Option<Vec<String>>,
    /// enclosure 直链（Mikan 即 `.torrent`；空 = 回退 `link`）。
    pub enclosure_url: String,
    /// 下载目标（enclosure / media:content，无 enclosure 时为回退链接）声明的 MIME 类型，
    /// 小写、不含参数（空 = feed 没声明）。引擎内部使用，据此识别无 `.torrent` 扩展名的
    /// 种子直链（`application/x-bittorrent`）。
    pub enclosure_type: String,
    /// 可选二段解析标识；非空时下载地址使用 `link`，由 resolver 获取精确资源。
    pub resolver_item: String,
    /// enclosure 声明大小（字节，0 = 未知）。
    pub enclosure_length: i64,
    /// 发布时间（Unix 秒，0 = 未知）。
    pub pub_date: i64,
    /// 首次入库时间（Unix 秒）。
    pub fetched_at: i64,
    /// 条目状态。
    pub status: RssItemStatus,
    /// `status == Downloaded` 时回链的任务 ID。
    pub task_id: String,
    /// 智能剧集归一键（空 = 未识别 / 未启用去重）。
    pub episode_key: String,
    /// 稳定的处置原因码（空 = 无）。取值见
    /// [`crate::rss::filter::RejectReason::code`] 与
    /// [`crate::rss::REASON_SEED_SKIPPED`]；**宿主负责本地化**，引擎不产出
    /// 面向用户的自然语言。
    pub reason: String,
}

impl RssItemInfo {
    /// 实际下载地址：带 `resolver_item` 时用条目 `link` 触发二段解析；否则优先
    /// `enclosure_url`，为空时回退 `link`（§2.2）。
    #[must_use]
    pub fn download_url(&self) -> &str {
        if !self.resolver_item.is_empty() && !self.link.is_empty() {
            return &self.link;
        }
        if self.enclosure_url.is_empty() {
            &self.link
        } else {
            &self.enclosure_url
        }
    }
}
