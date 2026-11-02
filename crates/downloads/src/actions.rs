//! 下载能力的键盘 / 菜单动作。键位与菜单装配由 app 完成；本 crate 只定义动作并
//! 在 [`crate::DownloadView`] 根元素上注册处理器。

use gpui::actions;

actions!(
    downloads,
    [
        NewDownload,
        OpenTorrentFile,
        /// 本机做种：把 daemon 主机上已有的文件 / 文件夹登记为已完成 BT 任务并做种。
        SeedLocal,
        PauseSelected,
        ResumeSelected,
        TogglePauseSelected,
        DeleteSelected,
        DeleteSelectedWithFiles,
        SelectAllTasks,
        /// 以当前选区进入显式多选模式（右键「选择」）。
        EnterMultiSelect,
        ClearSelection,
        PauseAll,
        ResumeAll,
        OpenSelected,
        RevealSelected,
        /// 把选中的已完成任务产物以文件写入系统剪贴板（右键「复制文件」）。
        CopySelectedFiles,
        OpenSelectedInWindow,
        /// 在停靠详情面板中查看选中任务（右键「详情」）。
        ShowSelectedDetail,
        CopySelectedUrl,
        /// 复制选中 BT 任务的磁力链接（右键「复制磁力链接」）。
        CopyMagnetSelected,
        /// 把选中 BT 任务的 .torrent 导出为文件（右键「导出 .torrent 文件…」）。
        ExportTorrentSelected,
        RenameSelected,
        /// 更换选中任务的下载链接（右键「更换链接」）。
        ChangeSelectedUrl,
        /// 修改选中任务的线程数（右键「修改线程数」）。
        ChangeSelectedSegments,
        /// 校验选中的已完成 BT 任务（右键「完整性校验」）。
        VerifySelected,
        RedownloadSelected,
        ToggleBoostSelected,
        FocusSearch,
        CycleDensity,
        CycleGroupBy,
        CycleSort,
        ToggleDetailPanel,
        OpenQueueManager,
        ClearFinished,
    ]
);

/// 下载页根元素的键位上下文名。
pub const KEY_CONTEXT: &str = "DownloadView";
