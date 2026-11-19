//! 任务实时传输状态与持久活动历史；不以分段数推算并发。

use serde::{Deserialize, Serialize};

/// 一个文件字节区间的进度，`active` 未知时不推断其传输状态。
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskSegmentDto {
    pub index: i32,
    pub start_byte: i64,
    pub end_byte: i64,
    pub downloaded_bytes: i64,
    pub active: Option<bool>,
}

/// 任务按加速来源累计的已写入字节：跨运行累加，重下 / 清盘时随进度一并复位。
///
/// 只记录源站主链路之外的路径；源站字节 = `downloaded_bytes` − 三者之和
/// （BT / ED2K 等 P2P 协议由客户端按协议整体归入 P2P）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskSourceBytesDto {
    /// 多 CDN 钉定节点（DoH / ECS 解析出的额外源站 IP）。
    #[serde(default)]
    pub cdn_bytes: i64,
    /// `ProxyMode::Auto` 下经候选代理路径（系统代理或手动代理）。
    #[serde(default)]
    pub proxy_bytes: i64,
    /// 多网卡聚合挂入的额外网卡链路。
    #[serde(default)]
    pub nic_bytes: i64,
}

/// HLS 点播媒体分片计数（视频与独立音轨合计）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct HlsFragmentProgress {
    pub written: u64,
    pub total: u64,
}

/// 任务最新采样；活跃传输不是物理 socket、配置上限或分段数。
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskRuntimeDto {
    pub task_id: String,
    pub sampled_at_ms: i64,
    /// 源端分配的单调采样序号；0 仅供未提供序号的旧客户端回退。
    #[serde(default)]
    pub sample_sequence: u64,
    pub active_transfers: Option<u32>,
    pub connected_peers: Option<u32>,
    pub parallelism_limit: Option<u32>,
    pub total_bytes: i64,
    pub segments: Vec<TaskSegmentDto>,
    /// 与字节区间分段无关；直播与旧主机不提供。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hls_fragments: Option<HlsFragmentProgress>,
    /// 本次采样时的加速来源累计（含在途连接）；`None` = 该协议 / 路径不做来源归因。
    #[serde(default)]
    pub source_bytes: Option<TaskSourceBytesDto>,
}

/// 持久任务事件；ID 在同一个下载数据库内单调递增。
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskActivityDto {
    pub id: i64,
    pub task_id: String,
    pub timestamp_ms: i64,
    pub kind: String,
    pub message: String,
    pub status: Option<i32>,
}

/// 默认返回最近一页；`before_id` 向前翻页，`after_id` 用于断线补齐，两者互斥。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskActivityQuery {
    pub task_id: String,
    #[serde(default)]
    pub before_id: Option<i64>,
    #[serde(default)]
    pub after_id: Option<i64>,
    /// 0 使用服务端默认值；服务端另设硬上限。
    #[serde(default)]
    pub limit: u32,
}

/// 页内按 ID 升序；保留范围用于标明历史被清理，不伪装成完整历史。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskActivityPage {
    pub entries: Vec<TaskActivityDto>,
    pub has_more: bool,
    pub oldest_id: Option<i64>,
    pub newest_id: Option<i64>,
    pub truncated: bool,
}

/// 查询单任务的速度历史（`daemon.task.speedHistory`）。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskSpeedHistoryQuery {
    pub task_id: String,
}

/// 任务整个下载期的速度曲线与统计，由 daemon 持久化，任务完成后仍可回看。
///
/// 横轴只计下载态（status=1）秒数：暂停、排队、做种时间不计入。曲线点数有上限，
/// 点数满额时相邻两点合并、`bucket_secs` 翻倍，因此任意时长的任务都覆盖全程。
/// 「重新下载」会清空历史；从未在记录功能上线后下载过的任务无历史（RPC 返回 `null`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskSpeedHistoryDto {
    pub task_id: String,
    /// 每个点覆盖的下载秒数（1、2、4…）。
    pub bucket_secs: u32,
    /// 每个点的平均速率（字节/秒），最旧在前；末点可能是仍在累积、不足 `bucket_secs` 秒的当前桶。
    pub samples: Vec<i64>,
    /// 累计下载秒数。
    pub active_secs: i64,
    /// 下载期间按逐秒速率累计的传输字节。
    pub transferred_bytes: i64,
    /// 逐秒速率峰值（字节/秒）。
    pub peak_bps: i64,
}

impl TaskSpeedHistoryDto {
    /// 下载期平均速率（字节/秒）；尚无下载秒数时为 0。
    ///
    /// ```
    /// use fluxdown_protocol::TaskSpeedHistoryDto;
    /// let history = TaskSpeedHistoryDto { active_secs: 4, transferred_bytes: 1000, ..Default::default() };
    /// assert_eq!(history.average_bps(), 250);
    /// assert_eq!(TaskSpeedHistoryDto::default().average_bps(), 0);
    /// ```
    #[must_use]
    pub fn average_bps(&self) -> i64 {
        if self.active_secs > 0 {
            self.transferred_bytes / self.active_secs
        } else {
            0
        }
    }
}
