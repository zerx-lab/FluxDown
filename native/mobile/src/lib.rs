//! FluxDown 移动端桥：UniFFI 绑定（Kotlin / 之后的 Swift）到真实主机。
//!
//! - 本机主机：进程内 `fluxdownd` + 嵌入式 agent（[`local`]）；
//! - 远端主机：`fluxdown-agent --server` 的 WebSocket `/rpc`（[`remote`]）；
//! - 会话核心（握手 / 快照 / 缓冲 / 游标 / 重同步 / 退避 / 离线宽限）传输无关
//!   （[`driver`] + [`projection`]），Kotlin 端只收到「已被接受」的信号。
//!
//! unsafe 审计边界：本 crate 的手写代码零 unsafe；`uniffi::setup_scaffolding!()` 与
//! `#[uniffi::export]` / `#[derive(uniffi::*)]` 宏展开中的 FFI 胶水（`unsafe impl FfiConverter`
//! 等）由用户批准作为 UniFFI 生成代码的审计边界（见 `rule://no-unsafe-in-rust`）。

mod driver;
mod dto;
mod error;
mod flux_core;
mod import;
mod link;
mod local;
mod projection;
mod remote;
pub mod sections;
mod session;
mod update;

#[cfg(test)]
mod flow_tests;
#[cfg(test)]
mod testkit;

pub use dto::{
    BtFileDto, CategoryDto, CloudDeviceDto, CreateTaskRequestDto, FileExistsActionDto, GroupDto,
    HlsFragmentProgressDto, HlsOptionDto, HostEventDto, HostInfoDto, HostSignalDto, HostSnapshotDto,
    LinkDeviceDto, QueueDto, RssSourceDto, RuntimeStatsDto, SegmentDto, SelectionKindDto,
    SelectionOutcomeDto, SelectionRequestDto, TaskDto, TaskRuntimeDto, VariantOptionDto,
};
pub use error::{ErrorCodeDto, FluxError, HostErrorDto};
pub use flux_core::FluxCore;
pub use import::{
    CREATE_CHUNK_SIZE, ImportBatchDto, ImportHandle, ImportItemDto, ImportProgressDto,
    ImportRetryDto, ImportSummaryDto, ImportUpdateDto, import_chunk_size,
};
pub use local::LocalHostConfig;
pub use session::HostSession;
pub use update::{
    AppReleaseNoteDto, AppUpdateConfig, AppUpdateSignalDto, AppUpdateStatusDto, AppUpdater,
    UpdateFailureDto, UpdateManualReasonDto, UpdatePhaseDto,
};

uniffi::setup_scaffolding!();
