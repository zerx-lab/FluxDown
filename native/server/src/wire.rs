//! WebSocket 与扩展 REST 端点的 wire JSON 契约（camelCase）。
//!
//! 与 `fluxdown_api::types` 同理：**不直接序列化** `EngineEvent` /
//! `engine::model::*` —— WS 协议一经发布即为对外稳定契约，引擎内部模型
//! 重构不得破坏线上 JSON 格式。转换集中在本模块的 `From` 实现。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use fluxdown_api::types::{QueueDto, TaskDto};
use fluxdown_engine::components::{FfmpegStatus, FfmpegVersions, YtdlpStatus, YtdlpVersions};
use fluxdown_engine::model::{
    BtFileEntry, HlsQualityOption, QueuePosition, ResolveVariantOption, SegmentDetail,
};

// ---------------------------------------------------------------------------
// WS 服务端 → 客户端
// ---------------------------------------------------------------------------

/// 分段字节范围与进度（`segmentProgress` 载荷）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SegmentDetailDto {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
}

impl From<SegmentDetail> for SegmentDetailDto {
    fn from(s: SegmentDetail) -> Self {
        Self {
            index: s.index,
            start_byte: s.start_byte,
            end_byte: s.end_byte,
            downloaded_bytes: s.downloaded_bytes,
        }
    }
}

/// 任务在队列中的位置（`queuePositionsChanged` 载荷）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueuePositionDto {
    pub task_id: String,
    /// 1-based，0 = 不在队列中。
    pub position: i32,
}

impl From<QueuePosition> for QueuePositionDto {
    fn from(p: QueuePosition) -> Self {
        Self {
            task_id: p.task_id,
            position: p.position,
        }
    }
}

/// HLS 可选码率变体（`hlsSelectionRequest` 载荷）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HlsQualityOptionDto {
    pub index: i32,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
}

impl From<HlsQualityOption> for HlsQualityOptionDto {
    fn from(o: HlsQualityOption) -> Self {
        Self {
            index: o.index,
            bandwidth: o.bandwidth,
            width: o.width,
            height: o.height,
        }
    }
}

/// 种子内单个文件条目（`btSelectionRequest` 载荷）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BtFileDto {
    pub index: i32,
    pub path: String,
    pub size: i64,
}

impl From<BtFileEntry> for BtFileDto {
    fn from(f: BtFileEntry) -> Self {
        Self {
            index: f.index,
            path: f.path,
            size: f.size,
        }
    }
}

/// 插件 resolve 返回的可选变体（`resolveVariantRequest` 载荷）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolveVariantOptionDto {
    pub index: i32,
    pub label: String,
    pub container: String,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
    pub total_bytes: i64,
}

impl From<ResolveVariantOption> for ResolveVariantOptionDto {
    fn from(o: ResolveVariantOption) -> Self {
        Self {
            index: o.index,
            label: o.label,
            container: o.container,
            bandwidth: o.bandwidth,
            width: o.width,
            height: o.height,
            total_bytes: o.total_bytes,
        }
    }
}

/// 服务端经 `/api/v1/ws` 推送的实时消息。
///
/// JSON 形态：`{"type":"taskProgress","taskId":"…",…}`（`type` 判别 + 扁平
/// camelCase 字段）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WsServerMsg {
    /// 任务进度（下载中周期推送；含 live speed —— REST `TaskDto` 无此字段）。
    TaskProgress {
        task_id: String,
        /// 0=pending, 1=downloading, 2=paused, 3=completed, 4=error, 5=preparing
        status: i32,
        downloaded_bytes: i64,
        total_bytes: i64,
        /// 字节/秒。
        speed: i64,
        file_name: String,
        save_dir: String,
        url: String,
        error_message: String,
    },
    /// 全部任务快照（连接建立时 + 引擎主动广播）。
    TasksSnapshot { tasks: Vec<TaskDto> },
    /// 分段级进度（详情面板分段可视化）。
    SegmentProgress {
        task_id: String,
        total_bytes: i64,
        segment_count: i32,
        segments: Vec<SegmentDetailDto>,
    },
    /// 动态分段拆分事件（驱动拆分动画）。
    SegmentSplit {
        task_id: String,
        parent_index: i32,
        parent_new_end: i64,
        child_index: i32,
        child_start: i64,
        child_end: i64,
        is_proactive: bool,
        total_segments: i32,
    },
    /// 任务元数据探测完成（文件名/大小确定）。
    TaskMetaProbed {
        task_id: String,
        file_name: String,
        total_bytes: i64,
    },
    /// 命名队列列表变化。
    QueuesChanged { queues: Vec<QueueDto> },
    /// 单任务队列归属变化（move_task_to_queue 定向广播）。
    TaskQueueChanged { task_id: String, queue_id: String },
    /// 队列内位置批量更新。
    QueuePositionsChanged { positions: Vec<QueuePositionDto> },
    /// Boost 优先任务变化。
    PriorityTaskChanged {
        priority_task_id: String,
        auto_paused_count: i32,
    },
    /// 请求客户端选择 HLS 画质（超时自动选最高带宽）。
    HlsSelectionRequest {
        task_id: String,
        options: Vec<HlsQualityOptionDto>,
    },
    /// 请求客户端选择 BT 文件（超时默认全部下载）。
    BtSelectionRequest {
        task_id: String,
        files: Vec<BtFileDto>,
    },
    /// 请求客户端选择插件 resolve 变体（画质/格式）；超时用插件提供的默认索引。
    ResolveVariantRequest {
        task_id: String,
        default_index: i32,
        options: Vec<ResolveVariantOptionDto>,
    },
    /// `ping` 应答（RTT 测量）。
    Pong {},
    /// 插件因熔断（连续超时/过载）被自动禁用（`reason` 固定 `"CircuitBreaker"`）。
    PluginAutoDisabled { identity: String, reason: String },
    /// 插件 onDone 钩子执行中（`running=true` 开始/`false` 结束）；同一任务可
    /// 有多个插件并发钩子，客户端按 `(taskId, pluginId)` 集合跟踪，用于在
    /// 已完成任务旁显示“插件处理中…”指示器。事件可能因 fire-and-forget 丢失
    /// （尤其是 `running=false`），客户端需自带看门狗超时兜底清除。
    PluginHookActivity {
        task_id: String,
        plugin_id: String,
        running: bool,
    },
    /// 插件表发生增删改（安装/卸载/启停/设置变更）；空载荷 ping，客户端收到后
    /// 全量 invalidate 插件列表查询。
    PluginsChanged {},
    /// 组件安装/下载进度（`component` 固定 `"ffmpeg"`；`totalBytes=0` 表示未知）。
    ComponentProgress {
        component: String,
        downloaded_bytes: i64,
        total_bytes: i64,
    },
    /// 组件安装/卸载操作结果（成功/失败 + 说明）。
    ComponentResult {
        component: String,
        ok: bool,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// WS 客户端 → 服务端
// ---------------------------------------------------------------------------

/// 客户端经 `/api/v1/ws` 发来的入站消息。
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WsClientMsg {
    /// 应答 `hlsSelectionRequest`。
    HlsSelection {
        task_id: String,
        selected_index: i32,
    },
    /// 应答 `btSelectionRequest`（空数组 = 全部文件）。
    BtSelection {
        task_id: String,
        selected_indices: Vec<i32>,
    },
    /// 应答 `resolveVariantRequest`。
    SelectVariant {
        task_id: String,
        selected_index: i32,
    },
    /// RTT 测量，服务端回 `pong`。
    Ping {},
}

// ---------------------------------------------------------------------------
// 扩展 REST 端点请求/响应体
// ---------------------------------------------------------------------------

/// 代理连通性测试请求（`POST /api/v1/proxy/test`）。
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestRequest {
    /// `http` / `https` / `socks4` / `socks5`。
    pub proxy_type: String,
    pub host: String,
    pub port: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// 代理连通性测试响应。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResponse {
    pub latency_ms: i64,
}

/// Tracker 订阅刷新结果（`POST /api/v1/bt/tracker-sub/refresh`）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrackerSubRefreshResponse {
    /// 至少一个订阅源拉取成功。
    pub success: bool,
    /// 去重合并后的唯一 Tracker 数。
    pub tracker_count: i64,
    /// 成功拉取的源数。
    pub ok_sources: i64,
    /// 尝试的订阅源总数。
    pub total_sources: i64,
    /// 缓存更新时间（Unix 秒；本次未成功时沿用旧值）。
    pub updated_at: i64,
    /// 全部源失败时的错误摘要（成功时为空）。
    pub error: String,
}

/// 创建命名队列请求（`POST /api/v1/queues`）。
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateQueueRequest {
    pub name: String,
    #[serde(default)]
    pub speed_limit_kbps: i64,
    #[serde(default)]
    pub max_concurrent: i32,
    #[serde(default)]
    pub default_save_dir: String,
    #[serde(default)]
    pub default_segments: i32,
    #[serde(default)]
    pub default_user_agent: String,
}

/// 更新命名队列请求（`PUT /api/v1/queues/{id}`），字段同创建。
pub type UpdateQueueRequest = CreateQueueRequest;

/// 移动任务到队列请求（`PUT /api/v1/tasks/{id}/queue`）。
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveQueueRequest {
    /// 空 = 默认队列。
    #[serde(default)]
    pub queue_id: String,
}

/// 队列每日定时计划请求（`PUT /api/v1/queues/{id}/schedule`）。
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueScheduleRequest {
    /// 定时计划是否启用。
    pub enabled: bool,
    /// 每日定时启动时间 `HH:MM`（空 = 不定时启动）。
    #[serde(default)]
    pub start_time: String,
    /// 每日定时停止时间 `HH:MM`（空 = 不定时停止）。
    #[serde(default)]
    pub stop_time: String,
    /// 生效星期位掩码：bit0=周一 … bit6=周日；0/缺省 = 每天。
    #[serde(default)]
    pub days: i32,
}

/// 队列内任务排序请求（`PUT /api/v1/queues/{id}/order`）。
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReorderQueueRequest {
    /// 队列内任务的完整新顺序（依次写入 1..N 的 queueOrder）。
    pub task_ids: Vec<String>,
}

/// 目录项（`FsListResponse.dirs` 元素）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub path: String,
}

/// 目录列举响应（`GET /api/v1/fs/list`，服务器端保存目录选择器用）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FsListResponse {
    /// 实际列举的目录（绝对路径）。
    pub path: String,
    /// 上级目录（根目录时为 None）。
    pub parent: Option<String>,
    /// 子目录列表（不含文件）。
    pub dirs: Vec<FsEntry>,
}

/// 服务器运行状态（`GET /api/v1/stats`，前端状态栏用）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatsResponse {
    /// 默认保存目录所在磁盘的剩余字节；探测失败为 None。
    pub disk_free_bytes: Option<u64>,
    pub save_dir: String,
    pub server_version: String,
    /// 当前 WS 连接数。
    pub ws_clients: usize,
    /// 演示模式开关（服务器以 `FLUXDOWN_DEMO_URL` 启动时为 true）。
    pub demo_mode: bool,
    /// 演示模式下唯一允许下载的 URL；非演示模式为空串。
    pub demo_url: String,
}

/// 单个日志文件（`GET /api/v1/logs` 列表项）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogFileDto {
    pub name: String,
    pub size: i64,
}

/// 日志目录与文件清单（`GET /api/v1/logs`，前端「关于」页展示 + 导出入口）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogsResponse {
    /// 日志目录绝对路径（NAS 用户据此在服务器文件系统定位日志）。
    pub dir: String,
    /// 全部日志文件（按日期 + 分卷序升序）。
    pub files: Vec<LogFileDto>,
}

/// token 重新生成响应（`POST /api/v1/token/regenerate`）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TokenResponse {
    pub token: String,
    /// 生效说明（新 token 需重启服务器后生效）。
    pub note: String,
}

// ---------------------------------------------------------------------------
// 组件（v1 仅 ffmpeg）
// ---------------------------------------------------------------------------

/// ffmpeg 组件状态（`GET /api/v1/components/ffmpeg`）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComponentFfmpegStatus {
    /// 生效路径来源：`manual` / `managed` / `system` / `none`。
    pub source: String,
    /// 生效的可执行文件路径（`source == "none"` 时为空）。
    pub path: String,
    /// `ffmpeg -version` 探测到的版本串（探测失败/未找到时为空）。
    pub version: String,
    /// 托管安装记录的版本号（空 = 未托管安装）。
    pub managed_version: String,
    /// 系统 PATH 中探测到的 ffmpeg 路径（无论是否生效；空 = 无）。
    pub system_path: String,
    /// 当前平台是否提供托管安装（BtbN 构建）。`false` = macOS 等——Web UI 隐藏
    /// 托管安装区块，只引导系统 PATH / 手动指定，避免反复弹「不支持安装」。
    pub managed_supported: bool,
}

impl From<FfmpegStatus> for ComponentFfmpegStatus {
    fn from(s: FfmpegStatus) -> Self {
        Self {
            source: s.source.as_str().to_string(),
            path: s.path,
            version: s.version,
            managed_version: s.managed_version,
            system_path: s.system_path,
            managed_supported: s.managed_supported,
        }
    }
}

/// ffmpeg 可安装版本列表（`GET /api/v1/components/ffmpeg/versions`）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComponentVersions {
    /// 降序排列的稳定版本号。
    pub versions: Vec<String>,
    /// 最新稳定版（= `versions` 首个；空 = 解析失败）。
    pub latest_stable: String,
}

impl From<FfmpegVersions> for ComponentVersions {
    fn from(v: FfmpegVersions) -> Self {
        Self {
            versions: v.versions,
            latest_stable: v.latest_stable,
        }
    }
}

/// yt-dlp 组件状态（`GET /api/v1/components/ytdlp`）。
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComponentYtdlpStatus {
    /// 生效路径来源：`manual` / `managed` / `system` / `none`。
    pub source: String,
    /// 生效的可执行文件路径（`source == "none"` 时为空）。
    pub path: String,
    /// `yt-dlp --version` 探测到的版本串（探测失败/未找到时为空）。
    pub version: String,
    /// 托管安装记录的版本号（空 = 未托管安装）。
    pub managed_version: String,
    /// 系统 PATH 中探测到的 yt-dlp 路径（无论是否生效；空 = 无）。
    pub system_path: String,
    /// 当前平台是否提供托管安装（GitHub Release 构建）。
    pub managed_supported: bool,
}

impl From<YtdlpStatus> for ComponentYtdlpStatus {
    fn from(s: YtdlpStatus) -> Self {
        Self {
            source: s.source.as_str().to_string(),
            path: s.path,
            version: s.version,
            managed_version: s.managed_version,
            system_path: s.system_path,
            managed_supported: s.managed_supported,
        }
    }
}

impl From<YtdlpVersions> for ComponentVersions {
    fn from(v: YtdlpVersions) -> Self {
        Self {
            versions: v.versions,
            latest_stable: v.latest_stable,
        }
    }
}

/// 安装/更新 ffmpeg 请求（`POST /api/v1/components/ffmpeg/install`）。
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstallFfmpegRequest {
    /// 钉住的版本号；`None` = 安装/更新到最新稳定版。
    #[serde(default)]
    pub version: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn ws_server_msg_uses_type_tag_and_camel_case() {
        let msg = WsServerMsg::TaskProgress {
            task_id: "t1".into(),
            status: 1,
            downloaded_bytes: 10,
            total_bytes: 100,
            speed: 5,
            file_name: "f.bin".into(),
            save_dir: "/tmp".into(),
            url: "http://x".into(),
            error_message: String::new(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"taskProgress\""));
        assert!(json.contains("\"taskId\":\"t1\""));
        assert!(json.contains("\"downloadedBytes\":10"));
    }

    #[test]
    fn ws_server_msg_plugin_hook_activity_uses_camel_case() {
        let msg = WsServerMsg::PluginHookActivity {
            task_id: "t1".into(),
            plugin_id: "p1".into(),
            running: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"pluginHookActivity\""));
        assert!(json.contains("\"taskId\":\"t1\""));
        assert!(json.contains("\"pluginId\":\"p1\""));
        assert!(json.contains("\"running\":true"));
    }

    #[test]
    fn ws_client_msg_roundtrip() {
        let msg: WsClientMsg =
            serde_json::from_str(r#"{"type":"hlsSelection","taskId":"t1","selectedIndex":2}"#)
                .unwrap();
        match msg {
            WsClientMsg::HlsSelection {
                task_id,
                selected_index,
            } => {
                assert_eq!(task_id, "t1");
                assert_eq!(selected_index, 2);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        let ping: WsClientMsg = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert!(matches!(ping, WsClientMsg::Ping {}));
    }

    #[test]
    fn ws_client_msg_select_variant_roundtrip() {
        let msg: WsClientMsg =
            serde_json::from_str(r#"{"type":"selectVariant","taskId":"t4","selectedIndex":1}"#)
                .unwrap();
        match msg {
            WsClientMsg::SelectVariant {
                task_id,
                selected_index,
            } => {
                assert_eq!(task_id, "t4");
                assert_eq!(selected_index, 1);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn ws_client_msg_bt_selection_roundtrip_with_indices() {
        let msg: WsClientMsg = serde_json::from_str(
            r#"{"type":"btSelection","taskId":"t2","selectedIndices":[0,2,5]}"#,
        )
        .unwrap();
        match msg {
            WsClientMsg::BtSelection {
                task_id,
                selected_indices,
            } => {
                assert_eq!(task_id, "t2");
                assert_eq!(selected_indices, vec![0, 2, 5]);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn ws_client_msg_bt_selection_empty_array_means_download_all() {
        // Per the `BtSelection` doc comment, an empty array is the wire
        // encoding for "download all files" -- it must deserialize to an
        // empty (not missing/defaulted) `Vec`, distinct from the field
        // being absent from the payload entirely.
        let msg: WsClientMsg =
            serde_json::from_str(r#"{"type":"btSelection","taskId":"t3","selectedIndices":[]}"#)
                .unwrap();
        match msg {
            WsClientMsg::BtSelection {
                task_id,
                selected_indices,
            } => {
                assert_eq!(task_id, "t3");
                assert!(selected_indices.is_empty());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    fn sample_task_dto(id: &str) -> TaskDto {
        TaskDto {
            task_id: id.to_string(),
            url: "http://example.com/file".into(),
            file_name: "video.mp4".into(),
            save_dir: "/downloads".into(),
            status: 1,
            downloaded_bytes: 10,
            total_bytes: 100,
            error_message: String::new(),
            created_at: "1700000000".into(),
            proxy_url: String::new(),
            queue_id: "q1".into(),
            checksum: String::new(),
            ignore_tls_errors: false,
            file_missing: false,
            completed_at: String::new(),
            referrer: String::new(),
        }
    }

    fn sample_queue_dto(id: &str) -> QueueDto {
        QueueDto {
            queue_id: id.to_string(),
            name: "工作队列".into(),
            speed_limit_kbps: 512,
            max_concurrent: 3,
            default_save_dir: "/downloads/work".into(),
            position: 1,
            default_segments: 4,
            default_user_agent: "FluxDown/1.0".into(),
            is_running: true,
            schedule_enabled: false,
            schedule_start: String::new(),
            schedule_stop: String::new(),
            schedule_days: 127,
        }
    }

    #[test]
    fn ws_server_msg_tasks_snapshot_variant() {
        let msg = WsServerMsg::TasksSnapshot {
            tasks: vec![sample_task_dto("task-1")],
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "tasksSnapshot");
        assert_eq!(v["tasks"][0]["taskId"], "task-1");
        assert_eq!(v["tasks"][0]["fileName"], "video.mp4");
        assert_eq!(v["tasks"][0]["downloadedBytes"], 10);
        assert_eq!(v["tasks"][0]["queueId"], "q1");
    }

    #[test]
    fn ws_server_msg_segment_progress_variant() {
        let msg = WsServerMsg::SegmentProgress {
            task_id: "t1".into(),
            total_bytes: 1000,
            segment_count: 2,
            segments: vec![
                SegmentDetailDto {
                    index: 0,
                    start_byte: 0,
                    end_byte: 500,
                    downloaded_bytes: 250,
                },
                SegmentDetailDto {
                    index: 1,
                    start_byte: 500,
                    end_byte: 1000,
                    downloaded_bytes: 100,
                },
            ],
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "segmentProgress");
        assert_eq!(v["taskId"], "t1");
        assert_eq!(v["totalBytes"], 1000);
        assert_eq!(v["segmentCount"], 2);
        assert_eq!(v["segments"][0]["startByte"], 0);
        assert_eq!(v["segments"][0]["endByte"], 500);
        assert_eq!(v["segments"][1]["downloadedBytes"], 100);
    }

    #[test]
    fn ws_server_msg_segment_split_variant() {
        let msg = WsServerMsg::SegmentSplit {
            task_id: "t1".into(),
            parent_index: 0,
            parent_new_end: 400,
            child_index: 2,
            child_start: 400,
            child_end: 800,
            is_proactive: true,
            total_segments: 3,
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "segmentSplit");
        assert_eq!(v["parentIndex"], 0);
        assert_eq!(v["parentNewEnd"], 400);
        assert_eq!(v["childIndex"], 2);
        assert_eq!(v["childStart"], 400);
        assert_eq!(v["childEnd"], 800);
        assert_eq!(v["isProactive"], true);
        assert_eq!(v["totalSegments"], 3);
    }

    #[test]
    fn ws_server_msg_task_meta_probed_variant() {
        let msg = WsServerMsg::TaskMetaProbed {
            task_id: "t1".into(),
            file_name: "movie.mkv".into(),
            total_bytes: 123_456,
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "taskMetaProbed");
        assert_eq!(v["fileName"], "movie.mkv");
        assert_eq!(v["totalBytes"], 123_456);
    }

    #[test]
    fn ws_server_msg_queues_changed_variant() {
        let msg = WsServerMsg::QueuesChanged {
            queues: vec![sample_queue_dto("q1")],
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "queuesChanged");
        assert_eq!(v["queues"][0]["queueId"], "q1");
        assert_eq!(v["queues"][0]["speedLimitKbps"], 512);
        assert_eq!(v["queues"][0]["defaultSaveDir"], "/downloads/work");
    }

    #[test]
    fn ws_server_msg_task_queue_changed_variant() {
        let msg = WsServerMsg::TaskQueueChanged {
            task_id: "t1".into(),
            queue_id: "later".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "taskQueueChanged");
        assert_eq!(v["taskId"], "t1");
        assert_eq!(v["queueId"], "later");
    }

    #[test]
    fn ws_server_msg_queue_positions_changed_variant() {
        let msg = WsServerMsg::QueuePositionsChanged {
            positions: vec![QueuePositionDto {
                task_id: "t1".into(),
                position: 3,
            }],
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "queuePositionsChanged");
        assert_eq!(v["positions"][0]["taskId"], "t1");
        assert_eq!(v["positions"][0]["position"], 3);
    }

    #[test]
    fn ws_server_msg_priority_task_changed_variant() {
        let msg = WsServerMsg::PriorityTaskChanged {
            priority_task_id: "t9".into(),
            auto_paused_count: 4,
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "priorityTaskChanged");
        assert_eq!(v["priorityTaskId"], "t9");
        assert_eq!(v["autoPausedCount"], 4);
    }

    #[test]
    fn ws_server_msg_hls_selection_request_variant() {
        let msg = WsServerMsg::HlsSelectionRequest {
            task_id: "t1".into(),
            options: vec![HlsQualityOptionDto {
                index: 0,
                bandwidth: 5_000_000,
                width: 1920,
                height: 1080,
            }],
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "hlsSelectionRequest");
        assert_eq!(v["taskId"], "t1");
        assert_eq!(v["options"][0]["bandwidth"], 5_000_000);
        assert_eq!(v["options"][0]["height"], 1080);
    }

    #[test]
    fn ws_server_msg_bt_selection_request_variant() {
        let msg = WsServerMsg::BtSelectionRequest {
            task_id: "t1".into(),
            files: vec![BtFileDto {
                index: 1,
                path: "folder/video.mp4".into(),
                size: 999,
            }],
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "btSelectionRequest");
        assert_eq!(v["files"][0]["path"], "folder/video.mp4");
        assert_eq!(v["files"][0]["size"], 999);
    }

    #[test]
    fn ws_server_msg_resolve_variant_request_variant() {
        let msg = WsServerMsg::ResolveVariantRequest {
            task_id: "t1".into(),
            default_index: 0,
            options: vec![ResolveVariantOptionDto {
                index: 0,
                label: "1080p MP4".into(),
                container: "mp4".into(),
                bandwidth: 5_000_000,
                width: 1920,
                height: 1080,
                total_bytes: 123_456,
            }],
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "resolveVariantRequest");
        assert_eq!(v["taskId"], "t1");
        assert_eq!(v["defaultIndex"], 0);
        assert_eq!(v["options"][0]["label"], "1080p MP4");
        assert_eq!(v["options"][0]["container"], "mp4");
        assert_eq!(v["options"][0]["totalBytes"], 123_456);
    }

    #[test]
    fn ws_server_msg_pong_variant_has_no_extra_fields() {
        let v: serde_json::Value = serde_json::to_value(&WsServerMsg::Pong {}).unwrap();
        assert_eq!(v, serde_json::json!({ "type": "pong" }));
    }
}
