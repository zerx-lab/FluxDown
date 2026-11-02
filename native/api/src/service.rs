//! API 宿主契约 —— [`ApiHost`] trait。
//!
//! HTTP 层只依赖本 trait，不关心宿主形态：冻结的 server 与
//! `fluxdown-agent` 都在各自边界实现能力，API crate 不依赖下载引擎。

use std::collections::HashMap;

use tokio::sync::broadcast;

use async_trait::async_trait;

use fluxdown_protocol::daemon::{
    BtGuardEntriesAddParams, BtGuardEntriesListDto, BtGuardEntriesListParams, BtGuardEntryDto,
    BtGuardEventsListDto, BtGuardEventsListParams, BtGuardStateDto, CreateGroupRequest,
    CreateTaskRequest, DownloadRequest, GroupDto, LinkAuth, LinkCodeResponse, LinkDeviceInfo,
    LinkDiscoveredPeer, LinkPairBeginResponse, LinkPairConfirmOutcome, LinkPairConfirmRequest,
    LinkPairHelloRequest, LinkPairHelloResponse, LinkPairRevealRequest, LinkPairRevealResponse,
    LinkPingInfo, MarketEntryDto, PluginAuthRequest, PluginAuthResponse, PluginDto, QueueDto,
    ResolvePreviewRequest, ResolvePreviewResponse, RssItemActionRequest, RssItemDto, RssSourceDto,
    RssValidateRequest, RssValidateResponse, SiteAuthCredentialDto, SiteAuthEntryDto,
    SiteAuthSaveRequest, TaskDto,
};

/// 404 fallback 响应的 message —— 请求命中了未注册的路由（例如管理 API 分组
/// 未启用时访问 `/api/v1/*`）。server 侧 fallback 与 CLI/客户端共用此常量，
/// 使「路由未注册」与「资源不存在（[`ApiError::NotFound`]，message `"not found"`）」
/// 两种 404 可被客户端区分，避免跨 crate 硬编码字符串漂移。
pub const UNKNOWN_ENDPOINT_MESSAGE: &str = "unknown endpoint";

/// 数据面链路鉴权失败的**诊断**原因（401 响应 JSON 的 `reason`）。响应本身未经认证，只用于排错，
/// 调用方**不得**据此持久化任何状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkAuthReason {
    /// 请求方未配对（本机名册里没有它，或它已被解除）。
    NotPaired,
    /// 请求时间戳与本机时钟相差超出容忍窗口。
    ClockSkew,
    /// 其它鉴权失败（方案头 / MAC / 重放 / 解密）。
    Unauthorized,
}

impl LinkAuthReason {
    /// 401 响应 JSON 里的 `reason` 取值。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotPaired => "notPaired",
            Self::ClockSkew => "clockSkew",
            Self::Unauthorized => "unauthorized",
        }
    }
}

/// API 操作错误。由 HTTP 层映射为响应状态码。
///
/// # Examples
///
/// ```
/// use fluxdown_api::service::ApiError;
///
/// let e = ApiError::BadRequest("url is required".to_string());
/// assert_eq!(e.to_string(), "url is required");
/// assert_eq!(ApiError::NotFound.to_string(), "not found");
/// ```
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 资源不存在 → 404。
    #[error("not found")]
    NotFound,
    /// 请求参数非法 → 400。
    #[error("{0}")]
    BadRequest(String),
    /// 宿主正在关闭（命令通道已断）→ 503。
    #[error("app shutting down")]
    Unavailable,
    /// 业务状态冲突（如重命名活跃任务）→ 409。消息为稳定错误码字符串，
    /// 原样透传给客户端做 i18n 映射。
    #[error("{0}")]
    Conflict(String),
    /// 鉴权失败（链路 HMAC 不符 / 设备未配对 / 时间戳过期）→ 401。
    #[error("unauthorized")]
    Unauthorized,
    /// 数据面链路鉴权失败 → 401，响应 JSON 额外带诊断 `reason` 与本机时间 `serverTime`。
    #[error("unauthorized")]
    LinkAuth(LinkAuthReason),
    /// 其它内部错误 → 500。
    #[error("{0}")]
    Internal(String),
}

/// API 宿主能力契约。所有方法与 `/api/v1` 端点一一对应，
/// 外加 [`submit_external`](ApiHost::submit_external)（接管/aria2 兼容入口）。
#[async_trait]
pub trait ApiHost: Send + Sync {
    /// 列出全部任务。
    async fn list_tasks(&self) -> Result<Vec<TaskDto>, ApiError>;

    /// 按 ID 查询单个任务。
    async fn get_task(&self, task_id: &str) -> Result<Option<TaskDto>, ApiError>;

    /// 直接创建下载任务（不弹确认框），返回新任务 ID。
    async fn create_task(&self, req: CreateTaskRequest) -> Result<String, ApiError>;

    /// 删除任务。`delete_files = true` 时同时删除磁盘文件。
    async fn delete_task(&self, task_id: &str, delete_files: bool) -> Result<(), ApiError>;

    /// 暂停单个任务。
    async fn pause_task(&self, task_id: &str) -> Result<(), ApiError>;

    /// 恢复单个任务。
    async fn continue_task(&self, task_id: &str) -> Result<(), ApiError>;

    /// 重命名任务文件（磁盘文件 + DB `file_name`，仅限非活跃的普通任务）。
    ///
    /// 错误消息为稳定错误码字符串（`invalid-name` / `task-active` /
    /// `bt-unsupported` / `not-found` / `target-exists`），宿主实现须按
    /// 状态映射：`not-found` → [`ApiError::NotFound`]、`invalid-name` →
    /// [`ApiError::BadRequest`]、其余业务拒绝 → [`ApiError::Conflict`]，
    /// 且除 `not-found` 外错误码字符串原样保留。默认实现返回不支持错误。
    async fn rename_task(&self, task_id: &str, file_name: &str) -> Result<(), ApiError> {
        let _ = (task_id, file_name);
        Err(ApiError::Internal(
            "task rename not supported by this host".to_string(),
        ))
    }

    /// 更换任务下载源地址（旧链接失效但内容不变，保留已下载进度续传）。
    ///
    /// 错误消息为稳定错误码字符串（`invalid-url` / `task-active` /
    /// `task-completed` / `bt-unsupported` / `not-found` /
    /// `protocol-unsupported` / `protocol-mismatch`，或 thunder 链接解码
    /// 失败的原文），宿主实现须按状态映射：`not-found` →
    /// [`ApiError::NotFound`]、`invalid-url` → [`ApiError::BadRequest`]、
    /// 其余业务拒绝 → [`ApiError::Conflict`]，且除 `not-found` 外错误码
    /// 字符串原样保留。默认实现返回不支持错误。
    async fn change_task_url(&self, task_id: &str, url: &str) -> Result<(), ApiError> {
        let _ = (task_id, url);
        Err(ApiError::Internal(
            "task url change not supported by this host".to_string(),
        ))
    }

    /// 暂停全部活跃任务。
    async fn pause_all(&self) -> Result<(), ApiError>;

    /// 恢复全部已暂停任务。
    async fn continue_all(&self) -> Result<(), ApiError>;

    /// 列出全部命名队列。
    async fn list_queues(&self) -> Result<Vec<QueueDto>, ApiError>;

    /// 提交一个外部下载请求（浏览器脚本接管 / aria2 兼容层）。
    ///
    /// 语义与浏览器扩展 NMH 一致：进入宿主的「外部下载」流程
    /// （桌面端会弹出快速下载确认框），**不**直接创建任务。
    async fn submit_external(&self, req: DownloadRequest) -> Result<(), ApiError>;

    /// 读取全局配置表快照（config key → value，FluxDown 原生键名）。
    ///
    /// aria2 兼容层（`getGlobalOption`）经此读取后翻译为 aria2 选项名。
    /// 默认实现返回空表（宿主未接线时兼容层按「无配置」降级）。
    async fn get_config(&self) -> Result<HashMap<String, String>, ApiError> {
        Ok(HashMap::new())
    }

    /// Web UI 默认语言（`en`/`zh`）。`/ping` 无鉴权透出，供未登录的前端
    /// 决定界面默认语言；每次请求实时求值，配置变更无需重启即可生效。
    /// 宿主自行决定配置表与部署环境（如 `FLUXDOWN_LANG`）的优先级。
    /// 默认实现返回 `None`（无 Web UI 的宿主，`/ping` 响应省略该字段）。
    async fn web_language(&self) -> Option<String> {
        None
    }

    /// 写入并 live-apply 一组配置键（FluxDown 原生键名 → 值）。
    ///
    /// 语义：先持久化到 config 表，再按键名热应用到引擎
    /// （镜像桌面 `SaveConfig` / server `ActorCmd::ApplyConfig` 路径）。
    /// 默认实现返回不支持错误。
    async fn apply_config(&self, changes: HashMap<String, String>) -> Result<(), ApiError> {
        let _ = changes;
        Err(ApiError::Internal(
            "config change not supported by this host".to_string(),
        ))
    }

    /// 列出已保存的站点凭据，只返回站点和用户名。
    async fn list_site_auth(&self) -> Result<Vec<SiteAuthEntryDto>, ApiError> {
        Ok(Vec::new())
    }

    /// 读取单站点凭据详情。该接口是定向受保护接口，不属于通用配置快照。
    async fn get_site_auth(&self, site: &str) -> Result<Option<SiteAuthCredentialDto>, ApiError> {
        let _ = site;
        Ok(None)
    }

    /// 保存单站点凭据。
    async fn save_site_auth(
        &self,
        request: SiteAuthSaveRequest,
    ) -> Result<SiteAuthEntryDto, ApiError> {
        let _ = request;
        Err(ApiError::Internal(
            "site auth change not supported by this host".to_string(),
        ))
    }

    /// 删除单站点凭据。
    async fn delete_site_auth(&self, site: &str) -> Result<(), ApiError> {
        let _ = site;
        Err(ApiError::Internal(
            "site auth change not supported by this host".to_string(),
        ))
    }

    /// 全部任务的实时速率快照（task_id → 速率）。
    ///
    /// 数据来自宿主对引擎进度事件的内存态缓存，不落库；
    /// 默认实现返回空表（兼容层按 0 速率降级）。
    async fn live_speeds(&self) -> Result<HashMap<String, LiveSpeed>, ApiError> {
        Ok(HashMap::new())
    }

    /// 订阅任务生命周期事件（aria2 WebSocket 通知源）。
    ///
    /// 宿主在自己的引擎事件槽（EventSink）里检测任务状态迁移并广播
    /// [`TaskEvent`]；jsonrpc 层把它翻译成 `aria2.onDownloadXxx` 通知帧。
    /// 默认实现返回 `None`（宿主未接线时 `/jsonrpc` 不提供 WS 通知）。
    fn subscribe_task_events(&self) -> Option<broadcast::Receiver<TaskEvent>> {
        None
    }

    // -- 插件系统（默认实现：未接线宿主降级；list 返回空表，写操作报不支持）--

    /// 列出全部已安装插件（含设置定义与当前值）。默认空表。
    async fn list_plugins(&self) -> Result<Vec<PluginDto>, ApiError> {
        Ok(Vec::new())
    }

    /// 启用/禁用插件（手动开关）。
    async fn set_plugin_enabled(&self, identity: &str, enabled: bool) -> Result<(), ApiError> {
        let _ = (identity, enabled);
        Err(plugins_unsupported())
    }

    /// 卸载插件。
    async fn uninstall_plugin(&self, identity: &str) -> Result<(), ApiError> {
        let _ = identity;
        Err(plugins_unsupported())
    }

    /// 批量更新插件设置（all-or-nothing）。
    async fn update_plugin_settings(
        &self,
        identity: &str,
        entries: HashMap<String, String>,
    ) -> Result<(), ApiError> {
        let _ = (identity, entries);
        Err(plugins_unsupported())
    }

    /// 驱动插件登录流程（二维码/账号登录）；默认宿主不提供插件运行时。
    async fn plugin_auth(
        &self,
        request: PluginAuthRequest,
    ) -> Result<PluginAuthResponse, ApiError> {
        let _ = request;
        Err(plugins_unsupported())
    }

    /// 从 zip 字节安装插件，返回 identity。
    async fn install_plugin_zip(&self, bytes: Vec<u8>) -> Result<String, ApiError> {
        let _ = bytes;
        Err(plugins_unsupported())
    }

    /// dev 模式安装（引用目录，不拷贝），返回 identity。
    async fn install_plugin_dev(&self, dir_path: String) -> Result<String, ApiError> {
        let _ = dir_path;
        Err(plugins_unsupported())
    }

    /// 任务级逃生舱：清除该任务的 resolver 绑定并按原始链接重跑。
    async fn ignore_plugin_retry(&self, task_id: &str) -> Result<(), ApiError> {
        let _ = task_id;
        Err(plugins_unsupported())
    }

    /// 拉取去中心化插件市场索引（多源 failover + 防回滚校验）。默认空表。
    async fn market_list(&self) -> Result<Vec<MarketEntryDto>, ApiError> {
        Ok(Vec::new())
    }

    /// 从市场安装某插件的最新版（下载 → content_hash 校验 → 安装），返回 identity。
    async fn market_install(&self, plugin_id: &str) -> Result<String, ApiError> {
        let _ = plugin_id;
        Err(plugins_unsupported())
    }

    /// 按插件声明权限探测缺失的基础组件（如 `"ffmpeg"`/`"ytdlp"`），供安装
    /// 成功后回填 [`fluxdown_protocol::daemon::InstalledPlugin::missing_components`] 提醒
    /// 用户安装依赖。依赖表见引擎 `plugin::dependencies`。默认空（无提醒）。
    async fn plugin_missing_components(&self, identity: &str) -> Vec<String> {
        let _ = identity;
        Vec::new()
    }

    // -- 任务组与前置预解析（Phase D：docs/multi-file-task-group-design.md）--
    // 默认实现降级为「未接线宿主」：resolve/list 返回空表，写操作报不支持——
    // 与插件系统的默认实现同一纪律，保 CLI 等纯客户端 `ApiHost` 实现不破。

    /// 前置预解析（多文件清单）：只读、不建任务、不写库。默认空结果
    /// （`items`/`error` 均空，客户端应回退普通单任务创建）。
    async fn resolve_preview(
        &self,
        req: ResolvePreviewRequest,
    ) -> Result<ResolvePreviewResponse, ApiError> {
        let _ = req;
        Ok(ResolvePreviewResponse {
            name: String::new(),
            source_url: String::new(),
            error: String::new(),
            plugin_id: String::new(),
            items: Vec::new(),
        })
    }

    /// 创建多文件任务组（建组 + N 子任务），返回新组 ID。
    async fn create_task_group(&self, req: CreateGroupRequest) -> Result<String, ApiError> {
        let _ = req;
        Err(groups_unsupported())
    }

    /// 列出全部任务组。默认空表。
    async fn list_groups(&self) -> Result<Vec<GroupDto>, ApiError> {
        Ok(Vec::new())
    }

    /// 暂停组内全部成员。宿主实现须先校验组存在，不存在 → [`ApiError::NotFound`]。
    async fn group_pause(&self, group_id: &str) -> Result<(), ApiError> {
        let _ = group_id;
        Err(groups_unsupported())
    }

    /// 恢复组内全部成员。宿主实现须先校验组存在，不存在 → [`ApiError::NotFound`]。
    async fn group_continue(&self, group_id: &str) -> Result<(), ApiError> {
        let _ = group_id;
        Err(groups_unsupported())
    }

    /// 删除整组（批量删成员）。`delete_files = true` 时同时删除磁盘文件。
    /// 宿主实现须先校验组存在，不存在 → [`ApiError::NotFound`]。
    async fn group_delete(&self, group_id: &str, delete_files: bool) -> Result<(), ApiError> {
        let _ = (group_id, delete_files);
        Err(groups_unsupported())
    }

    // -- RSS 订阅（默认降级：不支持的宿主返回空表 / 报错）--

    /// 列出全部 RSS 订阅（含派生的未读计数）。默认空表。
    async fn list_rss_sources(&self) -> Result<Vec<RssSourceDto>, ApiError> {
        Ok(Vec::new())
    }

    /// 新建订阅，返回新订阅 ID。`req.source_id` 忽略。
    async fn create_rss_source(&self, req: RssSourceDto) -> Result<String, ApiError> {
        let _ = req;
        Err(rss_unsupported())
    }

    /// 更新订阅配置。运行态字段忽略。订阅不存在 → [`ApiError::NotFound`]。
    async fn update_rss_source(&self, source_id: &str, req: RssSourceDto) -> Result<(), ApiError> {
        let _ = (source_id, req);
        Err(rss_unsupported())
    }

    /// 删除订阅（级联条目；已创建的下载任务保留）。
    async fn delete_rss_source(&self, source_id: &str) -> Result<(), ApiError> {
        let _ = source_id;
        Err(rss_unsupported())
    }

    /// 立即抓取一个订阅（异步派发，立即返回）。订阅正在抓取时视为成功（本次抓取即为结果），
    /// 仅订阅不存在返回 [`ApiError::NotFound`]。
    async fn refresh_rss_source(&self, source_id: &str) -> Result<(), ApiError> {
        let _ = source_id;
        Err(rss_unsupported())
    }

    /// 一个订阅的条目流（新→旧）。
    async fn list_rss_items(&self, source_id: &str) -> Result<Vec<RssItemDto>, ApiError> {
        let _ = source_id;
        Err(rss_unsupported())
    }

    /// 对条目执行手动操作（下载/忽略/全部标记已读）。
    async fn rss_item_action(
        &self,
        source_id: &str,
        req: RssItemActionRequest,
    ) -> Result<(), ApiError> {
        let _ = (source_id, req);
        Err(rss_unsupported())
    }

    /// 只读验证一个 feed 地址（新建订阅向导）。抓取失败不是 HTTP 错误——
    /// 失败原因进 [`RssValidateResponse::error`]。
    async fn validate_rss_feed(
        &self,
        req: RssValidateRequest,
    ) -> Result<RssValidateResponse, ApiError> {
        let _ = req;
        Err(rss_unsupported())
    }

    // -- BT 防吸血（默认降级：不支持的宿主报错）--

    /// 防吸血状态总览（开关 / 今日统计 / 云名单状态 / 过滤订阅 / 各层条目数）。
    async fn bt_guard_state(&self) -> Result<BtGuardStateDto, ApiError> {
        Err(bt_guard_unsupported())
    }

    /// 过滤条目分页。参数已由 HTTP 层校验（层 / 规则取值、`limit` 范围）。
    async fn bt_guard_entries_list(
        &self,
        params: BtGuardEntriesListParams,
    ) -> Result<BtGuardEntriesListDto, ApiError> {
        let _ = params;
        Err(bt_guard_unsupported())
    }

    /// 手动添加条目：`action == allow` 进白名单层，其余进手动层。CIDR 非法 →
    /// [`ApiError::BadRequest`]。
    async fn bt_guard_entries_add(
        &self,
        params: BtGuardEntriesAddParams,
    ) -> Result<BtGuardEntryDto, ApiError> {
        let _ = params;
        Err(bt_guard_unsupported())
    }

    /// 移除一个条目。条目不存在 → [`ApiError::NotFound`]；云名单层条目不可删 →
    /// [`ApiError::BadRequest`]（应改为加白）。
    async fn bt_guard_entry_remove(&self, entry_id: i64) -> Result<(), ApiError> {
        let _ = entry_id;
        Err(bt_guard_unsupported())
    }

    /// 事件日志分页（按时间降序）。
    async fn bt_guard_events_list(
        &self,
        params: BtGuardEventsListParams,
    ) -> Result<BtGuardEventsListDto, ApiError> {
        let _ = params;
        Err(bt_guard_unsupported())
    }

    // -- P2P 设备互联（默认降级：不支持的宿主报错 / ping 信息返回 None）--

    /// `/ping` 透出的本机设备互联身份（无鉴权）。默认 `None`（不支持 link 的宿主
    /// 省略该字段）。
    async fn link_ping_info(&self) -> Option<LinkPingInfo> {
        None
    }

    /// 处理入站配对 `hello`（无 token 鉴权，由一次性配对码守卫）。
    ///
    /// `source`：发起方的真实客户端地址（HTTP 层提取：默认取 `ConnectInfo` 的对端 IP；
    /// 仅当对端是 [`ApiHost::link_trusted_proxy`] 认可的反代时才采信
    /// `X-Forwarded-For` / `X-Real-IP`，取不到传 `None`）；透传给节流器做按来源
    /// 分桶计数，而非全局计数——避免一个恶意源的失败尝试连坐封锁同网段内所有正常
    /// 用户的配对，也避免反代后所有请求落进同一个桶。
    async fn link_pair_hello(
        &self,
        req: LinkPairHelloRequest,
        source: Option<std::net::IpAddr>,
    ) -> Result<LinkPairHelloResponse, ApiError> {
        let _ = (req, source);
        Err(link_unsupported())
    }

    /// 处理入站配对 `reveal`（无 token 鉴权，由 `link_pair_hello` 建立的会话 id 与发起方
    /// 先前的公钥承诺守卫）：发起方揭示临时公钥与随机数，宿主核对后返回响应方对完整握手
    /// 转录的签名，并向本机用户展示 SAS。
    async fn link_pair_reveal(
        &self,
        req: LinkPairRevealRequest,
    ) -> Result<LinkPairRevealResponse, ApiError> {
        let _ = req;
        Err(link_unsupported())
    }

    /// 处理入站配对 `confirm`（SAS 核对后确认/拒绝）。只有已揭示的会话才能 `confirm=true`。
    ///
    /// 返回 [`LinkPairConfirmOutcome`]：本机用户拒绝、核验超时都是**协议正常终局**，
    /// 必须以 2xx + `paired=false` + 判别串回给发起方，而不是压成 4xx——否则发起方
    /// 只能看到「会话过期」，无法告诉用户「对方拒绝了配对」。
    async fn link_pair_confirm(
        &self,
        req: LinkPairConfirmRequest,
    ) -> Result<LinkPairConfirmOutcome, ApiError> {
        let _ = req;
        Err(link_unsupported())
    }

    /// 批准/拒绝一次入站配对核验（本机作为响应方收到 `IncomingPairing` 通知后，
    /// 由用户在 UI 上核对 SAS 做出的决策）。
    async fn link_approve_incoming(&self, session_id: &str, accept: bool) -> Result<(), ApiError> {
        let _ = (session_id, accept);
        Err(link_unsupported())
    }

    /// 已配对设备下发下载任务：先校验 `auth`（链路 MAC，含 body 摘要与收发双方指纹），再用
    /// 链路密钥解密 `body`（原始请求体字节，须与签名覆盖的字节一致）为任务并创建。返回
    /// **已封装**的响应体（`{"taskId":…}` 的 AEAD 密文，HTTP 层原样回给对端并带
    /// `X-FluxLink-Enc: v2`）。
    async fn link_create_task(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        let _ = (auth, body);
        Err(link_unsupported())
    }

    /// 已配对设备经已认证链路双向交换设备信息：校验 `auth`（同 [`ApiHost::link_create_task`]），
    /// 并入请求方自报信息，返回已封装的本机信息（响应体原样二进制回给对端）。
    /// 默认不支持。
    async fn link_peer_info(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        let _ = (auth, body);
        Err(link_unsupported())
    }

    /// 已配对设备通知本机「我已解除与你的配对」：校验 `auth`（同 [`ApiHost::link_create_task`]），
    /// 把请求方记录标记为已撤销，返回已封装的确认响应。默认不支持。
    async fn link_unpair(&self, auth: LinkAuth, body: Vec<u8>) -> Result<Vec<u8>, ApiError> {
        let _ = (auth, body);
        Err(link_unsupported())
    }

    /// `peer` 是否是可信反向代理：是则 [`ApiHost::link_pair_hello`] 的来源地址取
    /// `X-Forwarded-For` / `X-Real-IP`。默认只信回环（同机反代）；明确配置了反代地址
    /// 的宿主（如 Docker 桥接网络里的 nginx）覆盖它。
    fn link_trusted_proxy(&self, peer: std::net::IpAddr) -> bool {
        peer.is_loopback()
    }

    /// 生成一次性配对码（供 headless 设备经 web/CLI 出示）。默认不支持。
    async fn link_generate_code(&self) -> Result<LinkCodeResponse, ApiError> {
        Err(link_unsupported())
    }

    /// 停止 mDNS 广播（撤销「可被发现」状态）。
    async fn link_stop_advertising(&self) -> Result<(), ApiError> {
        Err(link_unsupported())
    }

    // -- 本地互联管理面（web/PC 一致驱动 LinkManager；均需 management token，
    //    默认降级：不支持的宿主一律报 link_unsupported）--

    /// 本地设备发现开关：`start=true` 开始 mDNS 浏览并清空发现快照，
    /// `start=false` 停止。
    async fn link_discovery(&self, start: bool) -> Result<(), ApiError> {
        let _ = start;
        Err(link_unsupported())
    }

    /// 当前发现快照（发起方侧 UI 轮询用）。
    async fn link_discovered(&self) -> Result<Vec<LinkDiscoveredPeer>, ApiError> {
        Err(link_unsupported())
    }

    /// 手动地址探测（不入发现快照，直接返回给调用方）。
    async fn link_probe(&self, host: &str, port: u16) -> Result<LinkDiscoveredPeer, ApiError> {
        let _ = (host, port);
        Err(link_unsupported())
    }

    /// 发起配对：发送 `hello`，返回待确认令牌 + SAS + 对端信息。
    async fn link_pair_begin(
        &self,
        host: &str,
        port: u16,
        code: &str,
    ) -> Result<LinkPairBeginResponse, ApiError> {
        let _ = (host, port, code);
        Err(link_unsupported())
    }

    /// SAS 核对后确认/拒绝配对。`accept=false` 或对端拒绝 → `Ok(None)`。
    async fn link_pair_finish(
        &self,
        token: &str,
        accept: bool,
    ) -> Result<Option<LinkDeviceInfo>, ApiError> {
        let _ = (token, accept);
        Err(link_unsupported())
    }

    /// 全部已配对设备（含并发在线探测）。
    async fn link_devices(&self) -> Result<Vec<LinkDeviceInfo>, ApiError> {
        Err(link_unsupported())
    }

    /// 解除配对（删除设备）。不存在 → `Ok(false)`（handler 据此回 404）。
    async fn link_remove_device(&self, fingerprint: &str) -> Result<bool, ApiError> {
        let _ = fingerprint;
        Err(link_unsupported())
    }

    /// 把一个下载任务下发给已配对设备，返回对端新任务 ID。
    async fn link_dispatch(
        &self,
        fingerprint: &str,
        url: &str,
        save_dir: Option<&str>,
        file_name: Option<&str>,
    ) -> Result<String, ApiError> {
        let _ = (fingerprint, url, save_dir, file_name);
        Err(link_unsupported())
    }
}

/// 未支持插件的宿主（如纯 aria2 客户端场景）的统一错误。
fn plugins_unsupported() -> ApiError {
    ApiError::Internal("plugins not supported by this host".to_string())
}

/// 未支持任务组的宿主的统一错误。
fn groups_unsupported() -> ApiError {
    ApiError::Internal("task groups not supported by this host".to_string())
}

/// 未支持 RSS 订阅的宿主的统一错误。
fn rss_unsupported() -> ApiError {
    ApiError::Internal("rss subscriptions not supported by this host".to_string())
}

/// 未支持 BT 防吸血的宿主的统一错误。
fn bt_guard_unsupported() -> ApiError {
    ApiError::Internal("bt guard not supported by this host".to_string())
}

/// 未支持设备互联的宿主（如纯 aria2 客户端 / mobile）的统一错误。
pub fn link_unsupported() -> ApiError {
    ApiError::Internal("device link not supported by this host".to_string())
}

/// 任务生命周期事件类别，一一对应 aria2 的 6 种 WS 通知。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskEventKind {
    /// 任务开始/恢复下载 → `aria2.onDownloadStart`。
    Start,
    /// 任务被暂停 → `aria2.onDownloadPause`。
    Pause,
    /// 任务被用户删除/停止 → `aria2.onDownloadStop`。
    Stop,
    /// 任务下载完成 → `aria2.onDownloadComplete`。
    Complete,
    /// 任务出错 → `aria2.onDownloadError`。
    Error,
    /// BT 任务数据下载完成（可能仍在做种）→ `aria2.onBtDownloadComplete`。
    BtComplete,
}

/// 按「前态 → 新状态」判定应广播的 aria2 WS 通知事件类别（`None` = 不发，
/// 仅登记新状态）。纯函数，各 API 宿主共用同一份
/// 判定规则——历史上两端各自维护过一份，在 `next=2` 分支上曾经分叉
/// （server 旧实现无视 `prev` 一律发 `Pause`），此函数是唯一权威实现。
///
/// 规则：
/// - `prev == Some(next)`（状态未变，例如周期性进度上报反复携带同一
///   `status`）→ 不发，避免同一状态被重复通知。
/// - `next == 1`（downloading）且 `prev` ∈ `{None, Some(0), Some(2), Some(5)}`
///   （无历史/pending/paused/preparing）→ [`TaskEventKind::Start`]（含
///   unpause 恢复；aria2 对已存在 GID 重新开始下载同样再发一次
///   `onDownloadStart`）。`prev` 为 `Some(3)`/`Some(4)`（completed/error）
///   不在此列——这两个状态是 aria2 模型里 GID 的终点，「重新下载」对应新
///   GID，不会在同一 GID 上再见到 `onDownloadStart`。
/// - `next` ∈ `{2, 3, 4}` 且 `prev.is_some()`（非首次观测）→ 依次对应
///   [`TaskEventKind::Pause`]/[`TaskEventKind::Complete`]/
///   [`TaskEventKind::Error`]。
/// - `next` ∈ `{2, 3, 4}` 且 `prev.is_none()`（本进程内首次见到该任务、且
///   一上来就是终态/暂停态）→ 不发，只登记。覆盖场景：宿主重启后引擎为
///   已有任务重新上报的历史状态，避免快照风暴。
/// - `next == 0`（pending）或 `next == 5`（preparing）→ aria2 没有对应的
///   通知语义，不发。
/// - `Stop`/`BtComplete` 不经本函数判定：`Stop` 由调用方在删除命令处理点
///   直接广播；`BtComplete`（BT 数据下载完成但可能仍在做种）当前无法从
///   进度事件的字段判定这一子状态，不发送。
///
/// # Examples
///
/// ```
/// use fluxdown_api::service::{TaskEventKind, task_event_for_transition};
///
/// // 首次观测即下载中 → Start。
/// assert_eq!(task_event_for_transition(None, 1), Some(TaskEventKind::Start));
/// // 恢复下载 → Start。
/// assert_eq!(task_event_for_transition(Some(2), 1), Some(TaskEventKind::Start));
/// // 首次观测即暂停态 → 只登记，不发。
/// assert_eq!(task_event_for_transition(None, 2), None);
/// // 已知前态的暂停迁移 → Pause。
/// assert_eq!(task_event_for_transition(Some(1), 2), Some(TaskEventKind::Pause));
/// // 状态未变化 → 不重复触发。
/// assert_eq!(task_event_for_transition(Some(1), 1), None);
/// ```
pub fn task_event_for_transition(prev: Option<i32>, next: i32) -> Option<TaskEventKind> {
    if prev == Some(next) {
        return None;
    }
    match next {
        1 => matches!(prev, None | Some(0) | Some(2) | Some(5)).then_some(TaskEventKind::Start),
        2 if prev.is_some() => Some(TaskEventKind::Pause),
        3 if prev.is_some() => Some(TaskEventKind::Complete),
        4 if prev.is_some() => Some(TaskEventKind::Error),
        _ => None,
    }
}

/// 单条任务生命周期事件。见 [`ApiHost::subscribe_task_events`]。
#[derive(Debug, Clone)]
pub struct TaskEvent {
    /// FluxDown 任务 ID（UUID；jsonrpc 层负责转 GID）。
    pub task_id: String,
    /// 事件类别。
    pub kind: TaskEventKind,
}

/// 单任务实时速率（bytes/sec）。见 [`ApiHost::live_speeds`]。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveSpeed {
    /// 下载速率（bytes/sec）。
    pub download_bps: i64,
    /// 上传速率（bytes/sec，仅 BT 任务非 0）。
    pub upload_bps: i64,
}
