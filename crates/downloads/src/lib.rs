//! GPUI 下载能力页面、领域组件与视图状态。
//!
//! 本 crate 不负责窗口导航或应用初始化；宿主在 composition root 中创建
//! [`DownloadView`] 并作为路由内容注入 shell。

pub mod actions;
mod batch;
mod components;
mod controller;
mod deletion;
mod flags;
mod model;
mod pages;
mod strings;
mod submission;

pub use components::status_bar::{DownloadWidget, WidgetPlacement};
pub use components::title_bar::{
    DownloadPageWidget, DownloadSearchBox, DownloadTitleBar, DownloadViewMenu,
};
pub use controller::{
    BtGuardEvidence, DownloadsCommand, DownloadsController, DownloadsPort, DownloadsResult,
    LAST_DOWNLOAD_TARGET_PREF, LAST_SAVE_DIR_PREF, PortFuture, QueueFields,
    REMEMBER_LAST_SAVE_DIR_PREF, SeedLimits, TorrentShare,
};
pub use flags::{FlagAssets, flag_asset_path, flag_codes};
pub use model::devices::{DeviceEntry, DeviceKind, DispatchTarget};
pub use model::progress_window::{
    COMPLETION_WINDOW_PREF, MAX_TRACKED_WINDOW_TASKS, MainWindowState, PROGRESS_WINDOW_PREF,
    ProgressWindowEffect, ProgressWindowPrefs, ProgressWindowTracker, StartOrigin, UserStartIntent,
    WindowContext,
};
pub use model::save_dir_category::NewDownloadCategory;
pub use model::shutdown::*;
pub use pages::downloads::*;
pub use pages::file_conflict::*;
pub use pages::group_detail::*;
pub use pages::new_download::*;
pub use pages::progress_window::*;
pub use pages::queue_manager::*;
pub use pages::selection::*;
pub use pages::task_detail::*;
pub use submission::*;
