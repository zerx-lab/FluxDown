//! FFI 记录 / 枚举：与 Kotlin 端口（`mobile/Android/core` 的 `HostSession.kt` / `Models.kt` /
//! `Category.kt`）逐字段对应，并从 `fluxdown_protocol` 的 wire 类型一次性转换：
//! `BTreeMap` → `HashMap`、Unix 秒字符串 → `i64`、`serde_json::Value` 偏好 → 分类列表。
//!
//! 不直接给 `fluxdown_protocol` 类型加 `uniffi` 派生：协议 crate 必须保持零 unsafe、
//! 零传输依赖（见 `rule://no-unsafe-in-rust`）。

use std::collections::HashMap;

use fluxdown_protocol as proto;

use crate::error::HostErrorDto;

/// Unix 秒字符串 → `i64`；空串 / 非法 → 0（= 无）。
fn unix_secs(text: &str) -> i64 {
    text.trim().parse().unwrap_or(0)
}

/// `TaskDto`（不含速度 / 分段）。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct TaskDto {
    pub task_id: String,
    pub url: String,
    pub origin_url: String,
    pub file_name: String,
    pub save_dir: String,
    pub status: i32,
    pub downloaded_bytes: i64,
    pub total_bytes: i64,
    pub error_message: String,
    /// Unix 秒；0 = 无。
    pub created_at: i64,
    pub completed_at: i64,
    pub queue_id: String,
    pub group_id: String,
    pub rss_source_id: String,
    pub file_missing: bool,
    pub auto_route: String,
    pub source_cdn: i64,
    pub source_proxy: i64,
    pub source_nic: i64,
    pub uploaded_bytes: i64,
    pub seeding_status: i32,
    pub seeding_message: String,
    pub seeding_time_secs: i64,
}

impl From<&proto::TaskDto> for TaskDto {
    fn from(task: &proto::TaskDto) -> Self {
        Self {
            task_id: task.task_id.clone(),
            url: task.url.clone(),
            origin_url: task.origin_url.clone(),
            file_name: task.file_name.clone(),
            save_dir: task.save_dir.clone(),
            status: task.status,
            downloaded_bytes: task.downloaded_bytes,
            total_bytes: task.total_bytes,
            error_message: task.error_message.clone(),
            created_at: unix_secs(&task.created_at),
            completed_at: unix_secs(&task.completed_at),
            queue_id: task.queue_id.clone(),
            group_id: task.group_id.clone(),
            rss_source_id: task.rss_source_id.clone(),
            file_missing: task.file_missing,
            auto_route: task.auto_route.clone(),
            source_cdn: task.source_bytes.cdn_bytes,
            source_proxy: task.source_bytes.proxy_bytes,
            source_nic: task.source_bytes.nic_bytes,
            uploaded_bytes: task.uploaded_bytes,
            seeding_status: task.seeding_status,
            seeding_message: task.seeding_message.clone(),
            seeding_time_secs: task.seeding_time_secs,
        }
    }
}

/// `TaskSegmentDto`：`active = None` 表示未知，不得推断为空闲。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SegmentDto {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
    pub active: Option<bool>,
}

impl From<&proto::TaskSegmentDto> for SegmentDto {
    fn from(segment: &proto::TaskSegmentDto) -> Self {
        Self {
            index: segment.index,
            start_byte: segment.start_byte,
            end_byte: segment.end_byte,
            downloaded_bytes: segment.downloaded_bytes,
            active: segment.active,
        }
    }
}

/// `TaskRuntimeDto`。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct TaskRuntimeDto {
    pub task_id: String,
    pub sample_sequence: u64,
    pub active_transfers: Option<u32>,
    pub connected_peers: Option<u32>,
    pub total_bytes: i64,
    pub segments: Vec<SegmentDto>,
}

impl From<&proto::TaskRuntimeDto> for TaskRuntimeDto {
    fn from(runtime: &proto::TaskRuntimeDto) -> Self {
        Self {
            task_id: runtime.task_id.clone(),
            sample_sequence: runtime.sample_sequence,
            active_transfers: runtime.active_transfers,
            connected_peers: runtime.connected_peers,
            total_bytes: runtime.total_bytes,
            segments: runtime.segments.iter().map(SegmentDto::from).collect(),
        }
    }
}

/// `QueueDto`。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct QueueDto {
    pub queue_id: String,
    pub name: String,
    pub speed_limit_kbps: i64,
    pub upload_limit_kbps: i64,
    pub max_concurrent: i32,
    pub default_save_dir: String,
    pub position: i32,
    pub is_running: bool,
    pub schedule_enabled: bool,
    pub schedule_start: String,
    pub schedule_stop: String,
    /// bit0 = 周一 … bit6 = 周日；127 = 每天。
    pub schedule_days: i32,
}

impl From<&proto::QueueDto> for QueueDto {
    fn from(queue: &proto::QueueDto) -> Self {
        Self {
            queue_id: queue.queue_id.clone(),
            name: queue.name.clone(),
            speed_limit_kbps: queue.speed_limit_kbps,
            upload_limit_kbps: queue.upload_limit_kbps,
            max_concurrent: queue.max_concurrent,
            default_save_dir: queue.default_save_dir.clone(),
            position: queue.position,
            is_running: queue.is_running,
            schedule_enabled: queue.schedule_enabled,
            schedule_start: queue.schedule_start.clone(),
            schedule_stop: queue.schedule_stop.clone(),
            schedule_days: queue.schedule_days,
        }
    }
}

/// 排队任务的引擎排序键：任务留在队列期间不变，队首出队不改其余任务的键。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct QueueOrderDto {
    pub task_id: String,
    pub order: i64,
}

impl From<&proto::QueueOrderEntryDto> for QueueOrderDto {
    fn from(entry: &proto::QueueOrderEntryDto) -> Self {
        Self {
            task_id: entry.task_id.clone(),
            order: entry.order,
        }
    }
}

/// `GroupDto`；组进度由客户端汇总成员任务。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct GroupDto {
    pub group_id: String,
    pub name: String,
    pub source_url: String,
    pub save_dir: String,
    pub created_at: i64,
}

impl From<&proto::GroupDto> for GroupDto {
    fn from(group: &proto::GroupDto) -> Self {
        Self {
            group_id: group.group_id.clone(),
            name: group.name.clone(),
            source_url: group.source_url.clone(),
            save_dir: group.save_dir.clone(),
            created_at: unix_secs(&group.created_at),
        }
    }
}

/// `DaemonRuntimeStatsDto`。
#[derive(Clone, Debug, Default, PartialEq, uniffi::Record)]
pub struct RuntimeStatsDto {
    pub active_tasks: u32,
    pub pending_tasks: u32,
    pub total_download_bps: i64,
    pub total_upload_bps: i64,
    /// `None` = 探测失败。
    pub disk_free_bytes: Option<u64>,
    pub save_dir: String,
    pub retry_pending_tasks: u32,
}

impl From<&proto::DaemonRuntimeStatsDto> for RuntimeStatsDto {
    fn from(stats: &proto::DaemonRuntimeStatsDto) -> Self {
        Self {
            active_tasks: stats.active_tasks,
            pending_tasks: stats.pending_tasks,
            total_download_bps: stats.total_download_bps,
            total_upload_bps: stats.total_upload_bps,
            disk_free_bytes: stats.disk_free_bytes,
            save_dir: stats.save_dir.clone(),
            retry_pending_tasks: stats.retry_pending_tasks,
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct HlsOptionDto {
    pub index: i32,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct BtFileDto {
    pub index: i32,
    pub path: String,
    pub size: i64,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct VariantOptionDto {
    pub index: i32,
    pub label: String,
    pub container: String,
    pub bandwidth: i64,
    pub width: i64,
    pub height: i64,
    pub total_bytes: i64,
}

/// 多插件选择的候选插件（`PluginCandidateDto`）。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct PluginCandidateDto {
    pub identity: String,
    pub name: String,
    pub version: String,
    pub description: String,
}

/// `FileExistsAction`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FileExistsActionDto {
    Rename,
    Overwrite,
    Skip,
}

impl From<proto::FileExistsAction> for FileExistsActionDto {
    fn from(action: proto::FileExistsAction) -> Self {
        match action {
            proto::FileExistsAction::Rename => Self::Rename,
            proto::FileExistsAction::Overwrite => Self::Overwrite,
            proto::FileExistsAction::Skip => Self::Skip,
        }
    }
}

impl From<FileExistsActionDto> for proto::FileExistsAction {
    fn from(action: FileExistsActionDto) -> Self {
        match action {
            FileExistsActionDto::Rename => Self::Rename,
            FileExistsActionDto::Overwrite => Self::Overwrite,
            FileExistsActionDto::Skip => Self::Skip,
        }
    }
}

/// `SelectionKind`。
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum SelectionKindDto {
    Hls {
        options: Vec<HlsOptionDto>,
    },
    Bt {
        files: Vec<BtFileDto>,
        /// 任务当前保存目录（主机路径，供选择窗预填；旧 daemon 为空串）。
        save_dir: String,
    },
    Variant {
        options: Vec<VariantOptionDto>,
    },
    /// 保存目录里已有同名文件；字段由主机算好。
    FileExists {
        file_name: String,
        save_dir: String,
        existing_size: Option<u64>,
        existing_modified_unix_ms: Option<i64>,
        incoming_size: Option<i64>,
        rename_preview: String,
        actions: Vec<FileExistsActionDto>,
    },
    /// 多个插件都能处理该链接，由用户选择本次使用的插件（可记为站点默认）。
    Plugin {
        site: String,
        url: String,
        candidates: Vec<PluginCandidateDto>,
    },
}

/// `SelectionOutcome`。
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum SelectionOutcomeDto {
    Hls {
        index: i32,
    },
    Bt {
        indices: Vec<i32>,
        /// 非空且不同于当前目录时引擎改目录后重开；`None` = 沿用。
        save_dir: Option<String>,
    },
    Variant {
        index: i32,
    },
    FileExists {
        action: FileExistsActionDto,
    },
    Plugin {
        identity: String,
        remember: bool,
    },
    Cancelled,
}

impl From<&proto::SelectionKind> for SelectionKindDto {
    fn from(kind: &proto::SelectionKind) -> Self {
        match kind {
            proto::SelectionKind::Hls { options } => Self::Hls {
                options: options
                    .iter()
                    .map(|option| HlsOptionDto {
                        index: option.index,
                        bandwidth: option.bandwidth,
                        width: option.width,
                        height: option.height,
                    })
                    .collect(),
            },
            proto::SelectionKind::Bt { files, save_dir } => Self::Bt {
                files: files
                    .iter()
                    .map(|file| BtFileDto {
                        index: file.index,
                        path: file.path.clone(),
                        size: file.size,
                    })
                    .collect(),
                save_dir: save_dir.clone(),
            },
            proto::SelectionKind::Variant { options } => Self::Variant {
                options: options
                    .iter()
                    .map(|option| VariantOptionDto {
                        index: option.index,
                        label: option.label.clone(),
                        container: option.container.clone(),
                        bandwidth: option.bandwidth,
                        width: option.width,
                        height: option.height,
                        total_bytes: option.total_bytes,
                    })
                    .collect(),
            },
            proto::SelectionKind::FileExists {
                file_name,
                save_dir,
                existing_size,
                existing_modified_unix_ms,
                incoming_size,
                rename_preview,
                actions,
            } => Self::FileExists {
                file_name: file_name.clone(),
                save_dir: save_dir.clone(),
                existing_size: *existing_size,
                existing_modified_unix_ms: *existing_modified_unix_ms,
                incoming_size: *incoming_size,
                rename_preview: rename_preview.clone(),
                actions: actions.iter().copied().map(Into::into).collect(),
            },
            proto::SelectionKind::Plugin {
                site,
                url,
                candidates,
            } => Self::Plugin {
                site: site.clone(),
                url: url.clone(),
                candidates: candidates
                    .iter()
                    .map(|candidate| PluginCandidateDto {
                        identity: candidate.identity.clone(),
                        name: candidate.name.clone(),
                        version: candidate.version.clone(),
                        description: candidate.description.clone(),
                    })
                    .collect(),
            },
        }
    }
}

impl From<&proto::SelectionOutcome> for SelectionOutcomeDto {
    fn from(outcome: &proto::SelectionOutcome) -> Self {
        match outcome {
            proto::SelectionOutcome::Hls { index } => Self::Hls { index: *index },
            proto::SelectionOutcome::Bt { indices, save_dir } => Self::Bt {
                indices: indices.clone(),
                save_dir: save_dir.clone(),
            },
            proto::SelectionOutcome::Variant { index } => Self::Variant { index: *index },
            proto::SelectionOutcome::FileExists { action } => Self::FileExists {
                action: (*action).into(),
            },
            proto::SelectionOutcome::Plugin { identity, remember } => Self::Plugin {
                identity: identity.clone(),
                remember: *remember,
            },
            proto::SelectionOutcome::Cancelled => Self::Cancelled,
        }
    }
}

impl From<SelectionOutcomeDto> for proto::SelectionOutcome {
    fn from(outcome: SelectionOutcomeDto) -> Self {
        match outcome {
            SelectionOutcomeDto::Hls { index } => Self::Hls { index },
            SelectionOutcomeDto::Bt { indices, save_dir } => Self::Bt { indices, save_dir },
            SelectionOutcomeDto::Variant { index } => Self::Variant { index },
            SelectionOutcomeDto::FileExists { action } => Self::FileExists {
                action: action.into(),
            },
            SelectionOutcomeDto::Plugin { identity, remember } => {
                Self::Plugin { identity, remember }
            }
            SelectionOutcomeDto::Cancelled => Self::Cancelled,
        }
    }
}

/// 引擎发起的选择请求（X1–X3）。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SelectionRequestDto {
    pub request_id: String,
    pub task_id: String,
    pub kind: SelectionKindDto,
    pub default_choice: SelectionOutcomeDto,
    pub deadline_unix_ms: i64,
}

impl From<&proto::SelectionRequestDto> for SelectionRequestDto {
    fn from(request: &proto::SelectionRequestDto) -> Self {
        Self {
            request_id: request.request_id.clone(),
            task_id: request.task_id.clone(),
            kind: (&request.kind).into(),
            default_choice: (&request.default_choice).into(),
            deadline_unix_ms: request.deadline_unix_ms,
        }
    }
}

/// `ServiceHello` 的 UI 相关子集。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct HostInfoDto {
    pub service_name: String,
    pub service_version: String,
    pub protocol_version: u32,
    pub capabilities: Vec<String>,
}

impl From<&proto::ServiceHello> for HostInfoDto {
    fn from(hello: &proto::ServiceHello) -> Self {
        Self {
            service_name: hello.service_name.clone(),
            service_version: hello.service_version.clone(),
            protocol_version: hello.protocol_version,
            capabilities: hello.capabilities.clone(),
        }
    }
}

/// RSS 订阅（`RssSourceDto` 的 UI 子集）。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct RssSourceDto {
    pub source_id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub auto_download: bool,
    pub interval_minutes: i32,
    pub last_success_at: i64,
    pub last_error: String,
    pub fail_count: i32,
    pub unread_count: i32,
}

impl From<&proto::RssSourceDto> for RssSourceDto {
    fn from(source: &proto::RssSourceDto) -> Self {
        Self {
            source_id: source.source_id.clone(),
            name: source.name.clone(),
            url: source.url.clone(),
            enabled: source.enabled,
            auto_download: source.auto_download,
            interval_minutes: source.interval_minutes,
            last_success_at: source.last_success_at,
            last_error: source.last_error.clone(),
            fail_count: source.fail_count,
            unread_count: source.unread_count,
        }
    }
}

/// 设备自报的路径风格 → wire 名（`windows` / `posix`）；未上报或本端不认识为 `None`，
/// 由客户端按 `platform` 推断（同 `CloudDevice::effective_path_style`）。
fn path_style_wire(style: Option<proto::PathStyle>) -> Option<String> {
    match style? {
        proto::PathStyle::Windows => Some("windows".to_owned()),
        proto::PathStyle::Posix => Some("posix".to_owned()),
        proto::PathStyle::Unknown => None,
    }
}

/// 云端已信任设备（`CloudDevice` 子集）。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct CloudDeviceDto {
    pub device_id: String,
    pub name: String,
    pub platform: Option<String>,
    pub is_online: bool,
    pub is_current: bool,
    pub app_version: Option<String>,
    /// 设备自报的默认下载目录（远程下发不填保存目录时目标使用它）。
    pub default_save_dir: Option<String>,
    /// 见 [`path_style_wire`]。
    pub path_style: Option<String>,
}

impl From<&proto::CloudDevice> for CloudDeviceDto {
    fn from(device: &proto::CloudDevice) -> Self {
        Self {
            device_id: device.device_id.clone(),
            name: device.name.clone(),
            platform: device.platform.clone(),
            is_online: device.is_online,
            is_current: device.is_current,
            app_version: device.app_version.clone(),
            default_save_dir: device.default_save_dir.clone(),
            path_style: path_style_wire(device.path_style),
        }
    }
}

/// 局域网已配对设备（`LinkDeviceInfo` 子集）。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct LinkDeviceDto {
    pub fingerprint: String,
    pub name: String,
    pub platform: Option<String>,
    pub online: bool,
    /// 设备自报的默认下载目录（远程下发不填保存目录时目标使用它）。
    pub default_save_dir: Option<String>,
    /// 见 [`path_style_wire`]。
    pub path_style: Option<String>,
    /// 本机备注名（`agent.link.rename`）；`name` 已是生效名。
    pub custom_name: Option<String>,
    /// 对端自报的客户端版本。
    pub app_version: Option<String>,
    /// 对端自报的云设备 id：本机云名册里存在同 id 时合并为同一台设备展示。
    pub cloud_device_id: Option<String>,
    /// 对端已解除与本机的配对：不可下发，只能删除或重新配对。
    pub revoked: bool,
    /// 该设备被本机授权可经 `/rpc` 完全控制本机（本机用户批准了它的控制请求）。
    pub grants_control: bool,
    /// 本机可经 `/rpc` 控制该设备（本机以控制请求配对了它）。
    pub controllable: bool,
    /// 配对时间（Unix 秒）。
    pub paired_at: i64,
    /// 最近一次确认在线的时间（Unix 秒，0 = 未知）。
    pub last_seen_at: i64,
}

impl From<&proto::LinkDeviceInfo> for LinkDeviceDto {
    fn from(device: &proto::LinkDeviceInfo) -> Self {
        Self {
            fingerprint: device.fingerprint.clone(),
            name: device.name.clone(),
            platform: device.platform.clone(),
            online: device.online,
            default_save_dir: device.default_save_dir.clone(),
            path_style: path_style_wire(device.path_style),
            custom_name: device.custom_name.clone(),
            app_version: device.app_version.clone(),
            cloud_device_id: device.cloud_device_id.clone(),
            revoked: device.revoked,
            grants_control: device.grants_control,
            controllable: device.controllable,
            paired_at: device.paired_at,
            last_seen_at: device.last_seen_at,
        }
    }
}

/// 自定义分类（`CustomCategoryDto`，偏好键 `custom_categories`）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CategoryDto {
    pub id: String,
    /// 内置分类为空串：显示名走 i18n。
    pub name: String,
    pub icon: String,
    pub extensions: Vec<String>,
    /// 非空 = 正则模式（`matchMode = regex`）。
    pub regex_pattern: String,
    /// 非空 = 网址模式（`matchMode = url`）的通配符模式。
    pub url_pattern: String,
    pub position: i32,
    pub visible: bool,
    /// all / video / audio / document / image / program / archive / other；自定义为 `None`。
    pub builtin_type: Option<String>,
}

impl From<&proto::CustomCategoryDto> for CategoryDto {
    fn from(category: &proto::CustomCategoryDto) -> Self {
        let regex_mode = category.match_mode == "regex";
        Self {
            id: category.id.clone(),
            name: category.name.clone(),
            icon: category.icon.clone(),
            extensions: category.extensions.clone(),
            regex_pattern: if regex_mode {
                category.regex_pattern.clone()
            } else {
                String::new()
            },
            url_pattern: if category.match_mode == "url" {
                category.url_pattern.clone()
            } else {
                String::new()
            },
            position: i32::try_from(category.position).unwrap_or(if category.position < 0 {
                i32::MIN
            } else {
                i32::MAX
            }),
            visible: category.visible,
            builtin_type: category.builtin_type.clone(),
        }
    }
}

impl CategoryDto {
    /// 偏好 `custom_categories` → 分类列表（空 / 损坏已由协议层回退内置基线，按 position 排序）。
    pub(crate) fn from_preferences(preferences: &proto::AgentPreferencesDto) -> Vec<Self> {
        proto::CustomCategoryDto::from_preference(
            preferences.values.get(proto::CUSTOM_CATEGORIES_PREF_KEY),
        )
        .iter()
        .map(Self::from)
        .collect()
    }
}

/// `AgentSnapshot` 中移动端渲染的子集。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct HostSnapshotDto {
    pub info: HostInfoDto,
    pub daemon_connected: bool,
    /// 任务表是否仍在分页载入：`true` 期间 `tasks` / `runtime` / `queue_order` 为空，
    /// 只代表「尚未载入」，**不得**当成空列表（计数、空态、批量操作范围）。
    pub tasks_loading: bool,
    /// 主机上的任务总数：载入期间取主机报告的 `taskTotal`，载入完毕等于 `tasks.len()`。
    pub task_total: u64,
    pub tasks: Vec<TaskDto>,
    pub runtime: Vec<TaskRuntimeDto>,
    pub queues: Vec<QueueDto>,
    /// 排队任务的排序键，按 `order` 升序（名次 = 下标 + 1 即「排队 #n」）。
    pub queue_order: Vec<QueueOrderDto>,
    pub groups: Vec<GroupDto>,
    pub stats: RuntimeStatsDto,
    /// 0 或 1 个优先任务。
    pub priority_task_id: Option<String>,
    pub pending_selections: Vec<SelectionRequestDto>,
    /// `DaemonConfigSnapshot`：全部主机配置键（字符串值）。
    pub config: HashMap<String, String>,
    pub config_revision: u64,
    pub rss_sources: Vec<RssSourceDto>,
    pub cloud_devices: Vec<CloudDeviceDto>,
    pub link_devices: Vec<LinkDeviceDto>,
    pub categories: Vec<CategoryDto>,
    /// 其余 `AgentSnapshot` / `DaemonSnapshot` 分区：键见 [`crate::sections`]，值为协议 serde
    /// wire（camelCase）的 JSON 字符串。
    pub sections: HashMap<String, String>,
}

impl HostSnapshotDto {
    pub(crate) fn from_agent(
        info: &HostInfoDto,
        snapshot: &proto::AgentSnapshot,
        sections: HashMap<String, String>,
    ) -> Self {
        let daemon = &snapshot.daemon;
        let loading = daemon.tables_loading();
        Self {
            info: info.clone(),
            daemon_connected: snapshot.daemon_connected,
            tasks_loading: loading,
            task_total: u64::try_from(daemon.task_count()).unwrap_or(u64::MAX),
            // 载入期间投影里的表是半份（或已清空）：一律不下发，等载入完成后的完整快照。
            tasks: if loading {
                Vec::new()
            } else {
                daemon.tasks.iter().map(TaskDto::from).collect()
            },
            runtime: if loading {
                Vec::new()
            } else {
                daemon
                    .task_runtime
                    .values()
                    .map(TaskRuntimeDto::from)
                    .collect()
            },
            queues: daemon.queues.iter().map(QueueDto::from).collect(),
            queue_order: if loading {
                Vec::new()
            } else {
                queue_order_dtos(&daemon.queue_positions)
            },
            groups: daemon.groups.iter().map(GroupDto::from).collect(),
            stats: (&daemon.runtime_stats).into(),
            priority_task_id: daemon.priority.first().cloned(),
            pending_selections: daemon
                .pending_selections
                .iter()
                .map(SelectionRequestDto::from)
                .collect(),
            config: string_map(&daemon.config.values),
            config_revision: daemon.config.revision,
            rss_sources: daemon.rss_sources.iter().map(RssSourceDto::from).collect(),
            cloud_devices: snapshot
                .cloud_devices
                .iter()
                .map(CloudDeviceDto::from)
                .collect(),
            link_devices: snapshot
                .linked_devices
                .iter()
                .map(LinkDeviceDto::from)
                .collect(),
            categories: CategoryDto::from_preferences(&snapshot.preferences),
            sections,
        }
    }
}

/// 全量位置表 → 排序键表：协议保证按 `order` 升序，名次（`position`）不下发，由客户端按键序计算。
pub(crate) fn queue_order_dtos(positions: &[proto::QueuePositionDto]) -> Vec<QueueOrderDto> {
    positions
        .iter()
        .map(|entry| QueueOrderDto {
            task_id: entry.task_id.clone(),
            order: entry.order,
        })
        .collect()
}

pub(crate) fn string_map(
    values: &std::collections::BTreeMap<String, String>,
) -> HashMap<String, String> {
    values
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// 移动端订阅的事件子集（`DaemonEvent` / `WsServerMsg` 经 Rust 侧归一）。
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
// FFI 枚举形状由 Kotlin 端口约定；变体是短命值，不为 UniFFI 记录引入装箱。
#[allow(clippy::large_enum_variant)]
pub enum HostEventDto {
    TaskChanged {
        task: TaskDto,
    },
    TaskDeleted {
        task_id: String,
    },
    /// `WsServerMsg::TaskProgress`：速度只在这里出现。`status == 4 && error_message ==
    /// "deleted"` 的删除哨兵原样透传（Kotlin 端据此移除任务）。
    TaskProgress {
        task_id: String,
        status: i32,
        downloaded_bytes: i64,
        total_bytes: i64,
        speed: i64,
        upload_speed: i64,
        file_name: String,
        error_message: String,
        uploaded_bytes: i64,
        seeding_status: i32,
    },
    TaskRuntimeChanged {
        runtime: TaskRuntimeDto,
    },
    QueuesChanged {
        queues: Vec<QueueDto>,
    },
    /// 队列顺序整表替换（首次 / 用户重排）：按 `order` 升序，名次 = 下标 + 1。
    QueuePositionsChanged {
        order: Vec<QueueOrderDto>,
    },
    /// 队列顺序增量：先移除 `removed`，再按 `order` 插入 / 更新 `added`；名次由客户端按键序计算。
    QueueOrderChanged {
        removed: Vec<String>,
        added: Vec<QueueOrderDto>,
    },
    GroupsChanged {
        groups: Vec<GroupDto>,
    },
    FileMissingChanged {
        updates: HashMap<String, bool>,
    },
    PriorityTaskChanged {
        task_id: Option<String>,
    },
    RuntimeStatsChanged {
        stats: RuntimeStatsDto,
    },
    DaemonConnectionChanged {
        connected: bool,
    },
    SelectionPending {
        request: SelectionRequestDto,
    },
    SelectionResolved {
        request_id: String,
    },
    ConfigChanged {
        values: HashMap<String, String>,
        revision: u64,
    },
    RssSourcesChanged {
        sources: Vec<RssSourceDto>,
    },
    CloudDevicesChanged {
        devices: Vec<CloudDeviceDto>,
    },
    LinkedDevicesChanged {
        devices: Vec<LinkDeviceDto>,
    },
    CategoriesChanged {
        categories: Vec<CategoryDto>,
    },
    /// 通用分区变化（键见 [`crate::sections`]）：`json` 是该分区的完整新值。
    SectionChanged {
        name: String,
        json: String,
    },
    /// 一次性通知（不进快照）：`name` 为 serde 变体名，`json` 为载荷。
    Notice {
        name: String,
        json: String,
    },
}

/// 主机推送给 UI 仓库的信号。
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum HostSignalDto {
    /// 全量快照（连接 / 重同步）。
    Snapshot { snapshot: HostSnapshotDto },
    /// 已按游标规则接受的增量事件。
    Event { event: HostEventDto },
    /// 离线宽限（800ms）已过：数据只读、清空运行时。
    Stale,
    /// 不可恢复错误（协议不兼容、鉴权失败等）；之后 `next_signal` 返回 `None`。
    Fatal { error: HostErrorDto },
}

/// `CreateTaskRequest` 的移动端子集（N1 / N2）；空串 = 由引擎推断 / 跟随全局。
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct CreateTaskRequestDto {
    pub url: String,
    pub file_name: String,
    pub save_dir: String,
    /// 0 = 自动。
    pub segments: i32,
    pub queue_id: String,
    pub start_paused: bool,
    pub cookies: String,
    pub referrer: String,
    pub user_agent: String,
    pub proxy_url: String,
    pub checksum: String,
    pub ignore_tls_errors: bool,
    pub headers: HashMap<String, String>,
    pub http_user: String,
    pub http_password: String,
    pub save_site_auth: bool,
    /// 免打扰（静默）提交：引擎需要选择时不向 UI 请求而取默认。仅 `create_task` 使用；
    /// 批量导入忽略。
    pub unattended: bool,
    /// 新建下载表单 / 外部捕获的表单初始保存目录（任何分类预填之前的目录：跟随上次保存位置，
    /// 否则默认下载目录，否则主机运行时目录）。`None` = 永不按分类路由；`Some` 时由 Rust 按
    /// 已连接主机当前 `custom_categories` 偏好决定：`save_dir` 仍是系统给出的（空 / 初始目录 /
    /// 任一分类目录）则以初始目录为基准目录交给 daemon 按分类路由，用户改选了别的目录则不路由。
    pub offered_save_dir: Option<String>,
}

impl From<CreateTaskRequestDto> for proto::CreateTaskRequest {
    fn from(request: CreateTaskRequestDto) -> Self {
        Self {
            url: request.url,
            file_name: request.file_name,
            save_dir: request.save_dir,
            segments: request.segments,
            cookies: request.cookies,
            referrer: request.referrer,
            proxy_url: request.proxy_url,
            user_agent: request.user_agent,
            queue_id: request.queue_id,
            checksum: request.checksum,
            ignore_tls_errors: request.ignore_tls_errors,
            headers: (!request.headers.is_empty()).then_some(request.headers),
            torrent_b64: None,
            method: None,
            body: None,
            audio_url: None,
            start_paused: request.start_paused,
            http_user: request.http_user,
            http_password: request.http_password,
            save_site_auth: request.save_site_auth,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CategoryDto, SelectionKindDto, SelectionOutcomeDto, TaskDto, unix_secs};

    fn task_json(
        overrides: serde_json::Value,
    ) -> Result<fluxdown_protocol::TaskDto, serde_json::Error> {
        let mut base = json!({
            "taskId": "t1", "url": "https://example.com/a.bin", "fileName": "a.bin",
            "saveDir": "/data", "status": 1, "downloadedBytes": 5, "totalBytes": 10,
            "errorMessage": "", "createdAt": "1700000000", "proxyUrl": "", "queueId": "main",
            "checksum": ""
        });
        if let (Some(base), Some(extra)) = (base.as_object_mut(), overrides.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
        serde_json::from_value(base)
    }

    #[test]
    fn task_dto_parses_timestamps_and_flattens_source_bytes() -> Result<(), serde_json::Error> {
        let wire = task_json(json!({
            "completedAt": "1700000100",
            "originUrl": "https://origin/a",
            "sourceBytes": { "cdnBytes": 1, "proxyBytes": 2, "nicBytes": 3 }
        }))?;
        let dto = TaskDto::from(&wire);
        assert_eq!(dto.created_at, 1_700_000_000);
        assert_eq!(dto.completed_at, 1_700_000_100);
        assert_eq!(dto.origin_url, "https://origin/a");
        assert_eq!(
            (dto.source_cdn, dto.source_proxy, dto.source_nic),
            (1, 2, 3)
        );
        Ok(())
    }

    #[test]
    fn empty_or_invalid_timestamp_is_zero() {
        assert_eq!(unix_secs(""), 0);
        assert_eq!(unix_secs("not-a-number"), 0);
        assert_eq!(unix_secs(" 42 "), 42);
    }

    #[test]
    fn categories_come_from_preference_and_fall_back_to_builtin() {
        let mut preferences = fluxdown_protocol::AgentPreferencesDto::default();
        let builtin = CategoryDto::from_preferences(&preferences);
        assert_eq!(builtin.len(), 8);
        assert_eq!(builtin[0].builtin_type.as_deref(), Some("all"));

        let custom = json!([
            { "id": "c1", "name": "Docs", "icon": "file", "matchMode": "regex",
              "regexPattern": "\\.pdf$", "position": 2 },
            { "id": "c2", "name": "Pics", "matchMode": "extension",
              "extensions": ["png"], "regexPattern": "ignored", "position": 1 }
        ]);
        preferences.values.insert(
            fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY.to_owned(),
            serde_json::Value::String(custom.to_string()),
        );
        let list = CategoryDto::from_preferences(&preferences);
        assert_eq!(
            list.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["c2", "c1"]
        );
        assert_eq!(list[0].regex_pattern, "");
        assert_eq!(list[1].regex_pattern, "\\.pdf$");
    }

    #[test]
    fn bt_save_dir_maps_both_ways() {
        let kind = fluxdown_protocol::SelectionKind::Bt {
            files: vec![fluxdown_protocol::BtFileDto {
                index: 0,
                path: "a.bin".to_owned(),
                size: 12,
            }],
            save_dir: "/data".to_owned(),
        };
        let mapped = SelectionKindDto::from(&kind);
        assert!(matches!(
            mapped,
            SelectionKindDto::Bt { ref save_dir, .. } if save_dir == "/data"
        ));

        let outcome = SelectionOutcomeDto::Bt {
            indices: vec![0, 2],
            save_dir: Some("/new".to_owned()),
        };
        let wire = fluxdown_protocol::SelectionOutcome::from(outcome);
        assert_eq!(
            wire,
            fluxdown_protocol::SelectionOutcome::Bt {
                indices: vec![0, 2],
                save_dir: Some("/new".to_owned()),
            }
        );
        assert_eq!(
            SelectionOutcomeDto::from(&wire),
            SelectionOutcomeDto::Bt {
                indices: vec![0, 2],
                save_dir: Some("/new".to_owned()),
            }
        );
    }
}
