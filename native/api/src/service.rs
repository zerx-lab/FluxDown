//! API 宿主契约 —— [`ApiHost`] trait。
//!
//! HTTP 层（`server`/`jsonrpc` 模块）只依赖本 trait，不关心宿主形态：
//! - 桌面/手机 App（hub）：实现为「命令 + oneshot 回执」桥接到 download_actor
//! - 未来 headless server：实现为直接调用 `fluxdown_engine::Engine`
//!
//! 换宿主不换 HTTP 层，这是「一份 API 契约、多宿主复用」的核心边界。

use std::collections::HashMap;

use tokio::sync::broadcast;

use async_trait::async_trait;

use crate::types::{
    CreateGroupRequest, CreateTaskRequest, DownloadRequest, GroupDto, LinkAuth, LinkCodeResponse,
    LinkPairConfirmRequest, LinkPairHelloRequest, LinkPairHelloResponse, LinkPingInfo,
    MarketEntryDto, PluginDto, QueueDto, ResolvePreviewRequest, ResolvePreviewResponse, TaskDto,
};

/// 404 fallback 响应的 message —— 请求命中了未注册的路由（例如管理 API 分组
/// 未启用时访问 `/api/v1/*`）。server 侧 fallback 与 CLI/客户端共用此常量，
/// 使「路由未注册」与「资源不存在（[`ApiError::NotFound`]，message `"not found"`）」
/// 两种 404 可被客户端区分，避免跨 crate 硬编码字符串漂移。
pub const UNKNOWN_ENDPOINT_MESSAGE: &str = "unknown endpoint";

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
    /// 鉴权失败（链路 HMAC 不符 / 设备未配对 / 时间戳过期）→ 401。
    #[error("unauthorized")]
    Unauthorized,
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
    /// 成功后回填 [`crate::types::InstalledPlugin::missing_components`] 提醒
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

    // -- P2P 设备互联（默认降级：不支持的宿主报错 / ping 信息返回 None）--

    /// `/ping` 透出的本机设备互联身份（无鉴权）。默认 `None`（不支持 link 的宿主
    /// 省略该字段）。
    async fn link_ping_info(&self) -> Option<LinkPingInfo> {
        None
    }

    /// 处理入站配对 `hello`（无 token 鉴权，由一次性配对码守卫）。
    async fn link_pair_hello(
        &self,
        req: LinkPairHelloRequest,
    ) -> Result<LinkPairHelloResponse, ApiError> {
        let _ = req;
        Err(link_unsupported())
    }

    /// 处理入站配对 `confirm`（SAS 核对后确认/拒绝）。
    async fn link_pair_confirm(&self, req: LinkPairConfirmRequest) -> Result<(), ApiError> {
        let _ = req;
        Err(link_unsupported())
    }

    /// 已配对设备下发下载任务：先校验 `auth`（链路 HMAC，含 body 摘要），再反序列化
    /// `body`（原始请求体字节，须与签名覆盖的字节一致）为任务并创建，返回任务 ID。
    async fn link_create_task(&self, auth: LinkAuth, body: Vec<u8>) -> Result<String, ApiError> {
        let _ = (auth, body);
        Err(link_unsupported())
    }

    /// 生成一次性配对码（供 headless 设备经 web/CLI 出示）。默认不支持。
    async fn link_generate_code(&self) -> Result<LinkCodeResponse, ApiError> {
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

/// 未支持设备互联的宿主（如纯 aria2 客户端 / mobile）的统一错误。
fn link_unsupported() -> ApiError {
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
/// 仅登记新状态）。纯函数，宿主（hub 桌面端 / server headless 端）共用同一份
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
