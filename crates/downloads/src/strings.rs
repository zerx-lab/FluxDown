use fluxdown_ui_i18n::{Translator, keys};
use gpui::SharedString;

/// 「聚焦搜索」快捷键提示（与 app `menus.rs` 的 `secondary-f` 绑定一致），显示在顶栏搜索框右侧徽标里。
#[cfg(target_os = "macos")]
pub(crate) const SEARCH_SHORTCUT_HINT: &str = "⌘F";
#[cfg(not(target_os = "macos"))]
pub(crate) const SEARCH_SHORTCUT_HINT: &str = "Ctrl+F";

/// 组计数行一段的语气：下载中用强调色，失败用破坏色，其余为三级文字。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupCountTone {
    Plain,
    Active,
    Failed,
}

#[derive(Clone)]
pub(crate) struct DownloadStrings {
    pub(crate) category_archive: SharedString,
    pub(crate) category_audio: SharedString,
    pub(crate) category_document: SharedString,
    pub(crate) category_image: SharedString,
    pub(crate) category_other: SharedString,
    pub(crate) category_program: SharedString,
    pub(crate) category_video: SharedString,
    pub(crate) col_created: SharedString,
    pub(crate) col_eta: SharedString,
    pub(crate) col_file_name: SharedString,
    pub(crate) col_progress: SharedString,
    pub(crate) col_protocol: SharedString,
    pub(crate) col_queue: SharedString,
    pub(crate) col_size: SharedString,
    pub(crate) col_source: SharedString,
    pub(crate) col_speed: SharedString,
    pub(crate) col_status: SharedString,
    pub(crate) active_transfers: SharedString,
    pub(crate) connected_peers: SharedString,
    pub(crate) delete: SharedString,
    pub(crate) delete_task: SharedString,
    pub(crate) delete_task_and_file: SharedString,
    pub(crate) clean_stale_tasks: SharedString,
    pub(crate) clean_stale_tasks_description: SharedString,
    delete_confirm_with_file: SharedString,
    delete_confirm_keep_file: SharedString,
    batch_delete_confirm_with_file: SharedString,
    pub(crate) too_many_windows_hint: SharedString,
    pub(crate) confirm: SharedString,
    pub(crate) cancel: SharedString,
    import_progress: SharedString,
    import_retrying: SharedString,
    pub(crate) import_cancelling: SharedString,
    delete_progress: SharedString,
    delete_cancelled: SharedString,
    delete_partial: SharedString,
    deleted_count: SharedString,
    pub(crate) disconnected: SharedString,
    pub(crate) action_failed: SharedString,
    pub(crate) metadata_loading: SharedString,
    subtitle_queued: SharedString,
    eta_seconds: SharedString,
    eta_minutes: SharedString,
    eta_hours: SharedString,
    pub(crate) later_queue: SharedString,
    pub(crate) pause: SharedString,
    pub(crate) pause_all: SharedString,
    pub(crate) resume: SharedString,
    pub(crate) resume_all: SharedString,
    /// 状态栏批量按钮禁用时的 tooltip：说明为什么当前不可点。
    pub(crate) pause_all_unavailable: SharedString,
    pub(crate) resume_all_unavailable: SharedString,
    pub(crate) resume_all_queue_stopped: SharedString,
    pub(crate) main_queue: SharedString,
    pub(crate) open_file: SharedString,
    pub(crate) open_folder: SharedString,
    /// 右键「复制文件」：把产物以文件写入系统剪贴板。
    pub(crate) copy_file: SharedString,
    pub(crate) remote_tasks: SharedString,
    pub(crate) sidebar_queues: SharedString,
    pub(crate) status_all: SharedString,
    pub(crate) status_completed: SharedString,
    pub(crate) status_downloading: SharedString,
    pub(crate) status_error: SharedString,
    pub(crate) task_error_post_only_endpoint: SharedString,
    pub(crate) task_error_post_only_resume: SharedString,
    pub(crate) status_incomplete: SharedString,
    pub(crate) status_paused: SharedString,
    pub(crate) status_seeding: SharedString,
    pub(crate) seeding_status_queued: SharedString,
    pub(crate) status_pending: SharedString,
    pub(crate) today: SharedString,
    pub(crate) yesterday: SharedString,
    pub(crate) this_week: SharedString,
    pub(crate) this_month: SharedString,
    pub(crate) older: SharedString,
    pub(crate) site_bt: SharedString,
    pub(crate) ignore_plugin_retry: SharedString,
    pub(crate) ignore_plugin_retry_title: SharedString,
    pub(crate) ignore_plugin_retry_msg: SharedString,
    pub(crate) boost_download: SharedString,
    pub(crate) cancel_boost: SharedString,
    /// 对已完成任务点「优先下载」的提示。
    pub(crate) boost_task_completed: SharedString,
    /// 对已失败（已结束）任务点「优先下载」的提示。
    pub(crate) boost_task_ended: SharedString,
    pub(crate) rename_task: SharedString,
    pub(crate) rename_task_title: SharedString,
    pub(crate) rename_task_placeholder: SharedString,
    /// 右键「更换链接」及其对话框。
    pub(crate) change_url: SharedString,
    pub(crate) change_url_title: SharedString,
    pub(crate) change_url_label: SharedString,
    pub(crate) change_url_placeholder: SharedString,
    /// 右键「修改线程数」及其对话框（含「已下进度保留」说明与成功提示）。
    pub(crate) edit_threads: SharedString,
    pub(crate) edit_threads_title: SharedString,
    pub(crate) edit_threads_hint: SharedString,
    pub(crate) edit_threads_label: SharedString,
    pub(crate) edit_threads_zero_hint: SharedString,
    pub(crate) edit_threads_invalid: SharedString,
    pub(crate) threads_changed: SharedString,
    /// 「本机做种」对话框与入口。
    pub(crate) seed_local_title: SharedString,
    pub(crate) seed_local_desc: SharedString,
    pub(crate) seed_local_path: SharedString,
    pub(crate) seed_local_path_placeholder: SharedString,
    pub(crate) seed_local_pick_file: SharedString,
    pub(crate) seed_local_pick_folder: SharedString,
    pub(crate) seed_local_torrent: SharedString,
    pub(crate) seed_local_torrent_hint: SharedString,
    pub(crate) seed_local_pick_torrent: SharedString,
    pub(crate) seed_local_trackers: SharedString,
    pub(crate) seed_local_trackers_hint: SharedString,
    pub(crate) seed_local_unlimited: SharedString,
    pub(crate) seed_local_unlimited_desc: SharedString,
    pub(crate) seed_local_submit: SharedString,
    pub(crate) seed_local_working: SharedString,
    pub(crate) seed_local_started: SharedString,
    pub(crate) seed_local_err_path_required: SharedString,
    /// 右键「完整性校验」（已完成 BT 任务）。
    pub(crate) verify_integrity: SharedString,
    pub(crate) redownload_task: SharedString,
    pub(crate) copy_url: SharedString,
    pub(crate) copy_magnet: SharedString,
    pub(crate) export_torrent: SharedString,
    /// 复制下载地址后的成功提示。
    pub(crate) url_copied: SharedString,
    pub(crate) move_to_queue: SharedString,
    /// 右键「选择」：以当前选区进入多选模式。
    pub(crate) select_tasks: SharedString,
    /// 右键「全选」。
    pub(crate) select_all_tasks: SharedString,
    /// 行首图标格悬停复选框的提示。
    pub(crate) select_task_tooltip: SharedString,
    /// 右键「详情」：在停靠面板中查看。
    pub(crate) detail: SharedString,
    /// 详情面板头部「在独立窗口打开」。
    pub(crate) open_in_window: SharedString,
    pub(crate) group_pause_all: SharedString,
    pub(crate) group_resume_all: SharedString,
    pub(crate) group_retry_failed: SharedString,
    pub(crate) group_open_folder: SharedString,
    pub(crate) group_copy_source_link: SharedString,
    pub(crate) group_delete: SharedString,
    pub(crate) group_delete_with_files: SharedString,
    group_delete_confirm_with_file: SharedString,
    pub(crate) open_group_in_window: SharedString,
    group_items_count: SharedString,
    group_done_count: SharedString,
    group_downloading_count: SharedString,
    group_pending_count: SharedString,
    group_paused_count: SharedString,
    group_failed_count: SharedString,
    group_done_of_total: SharedString,
    group_eta_remaining: SharedString,
    group_dir_count: SharedString,
    group_detail_subtitle: SharedString,
    pub(crate) group_detail_source: SharedString,
    pub(crate) group_detail_save_dir: SharedString,
    pub(crate) group_detail_created_at: SharedString,
    pub(crate) group_detail_queue: SharedString,
    pub(crate) search_tasks_placeholder: SharedString,
    pub(crate) unsupported_drop_hint: SharedString,
    pub(crate) sidebar_status: SharedString,
    pub(crate) sidebar_devices: SharedString,
    pub(crate) device_download_to_this: SharedString,
    pub(crate) linked_device_rename: SharedString,
    pub(crate) linked_device_unpair: SharedString,
    pub(crate) device_presence_unknown: SharedString,
    pub(crate) this_device: SharedString,
    pub(crate) all_devices: SharedString,
    pub(crate) add_device: SharedString,
    pub(crate) status_canceled: SharedString,
    pub(crate) status_file_missing: SharedString,
    pub(crate) conflict_pending: SharedString,
    pub(crate) conflict_pending_tooltip: SharedString,
    pub(crate) add_category: SharedString,
    pub(crate) edit_category: SharedString,
    pub(crate) hide_section: SharedString,
    pub(crate) start_queue_action: SharedString,
    pub(crate) stop_queue_action: SharedString,
    pub(crate) manage_queue_action: SharedString,
    pub(crate) delete_queue_action: SharedString,
    pub(crate) queue_delete_confirm_desc: SharedString,
    pub(crate) tab_failed: SharedString,
    pub(crate) empty_title: SharedString,
    pub(crate) empty_subtitle: SharedString,
    /// 任务表分页载入期间的标题 / 说明模板（`{total}` = 服务端报告的任务总数）。
    pub(crate) tasks_loading_title: SharedString,
    tasks_loading_subtitle: SharedString,
}

impl DownloadStrings {
    pub(crate) fn from_translator(translator: &Translator) -> Self {
        Self {
            category_archive: shared(translator.text(keys::CATEGORY_ARCHIVE)),
            category_audio: shared(translator.text(keys::CATEGORY_AUDIO)),
            category_document: shared(translator.text(keys::CATEGORY_DOCUMENT)),
            category_image: shared(translator.text(keys::CATEGORY_IMAGE)),
            category_other: shared(translator.text(keys::CATEGORY_OTHER)),
            category_program: shared(translator.text(keys::CATEGORY_PROGRAM)),
            category_video: shared(translator.text(keys::CATEGORY_VIDEO)),
            col_created: shared(translator.text(keys::COL_CREATED)),
            col_eta: shared(translator.text(keys::COL_ETA)),
            col_file_name: shared(translator.text(keys::COL_FILE_NAME)),
            col_progress: shared(translator.text("colProgress")),
            col_protocol: shared(translator.text("colProtocol")),
            col_queue: shared(translator.text("colQueue")),
            col_size: shared(translator.text(keys::COL_SIZE)),
            col_source: shared(translator.text("colSource")),
            col_speed: shared(translator.text(keys::COL_SPEED)),
            active_transfers: shared(translator.text("taskActiveTransfers")),
            connected_peers: shared(translator.text("taskConnectedPeers")),
            confirm: shared(translator.text("confirm")),
            empty_title: shared(translator.text("emptyTitle")),
            empty_subtitle: shared(translator.text("emptySubtitle")),
            tasks_loading_title: shared(translator.text("tasksLoadingTitle")),
            tasks_loading_subtitle: shared(translator.text("tasksLoadingSubtitle")),
            cancel: shared(translator.text("cancel")),
            import_progress: shared(translator.text("importProgress")),
            import_retrying: shared(translator.text("importRetrying")),
            import_cancelling: shared(translator.text("importCancelling")),
            delete_progress: shared(translator.text("deleteProgress")),
            delete_cancelled: shared(translator.text("deleteCancelled")),
            delete_partial: shared(translator.text("deletePartial")),
            deleted_count: shared(translator.text("mobileToastDeletedN")),
            disconnected: shared(translator.text("localServiceDisconnected")),
            action_failed: shared(translator.text("localServiceActionFailed")),
            metadata_loading: shared(translator.text("statusPreparing")),
            subtitle_queued: shared(translator.text("subtitleQueued")),
            eta_seconds: shared(translator.text("etaSeconds")),
            eta_minutes: shared(translator.text("etaMinutes")),
            eta_hours: shared(translator.text("etaHours")),
            col_status: shared(translator.text(keys::COL_STATUS)),
            later_queue: shared(translator.text(keys::LATER_QUEUE)),
            delete: shared(translator.text(keys::DELETE)),
            delete_task: shared(translator.text("deleteTask")),
            delete_task_and_file: shared(translator.text("deleteTaskAndFile")),
            clean_stale_tasks: shared(translator.text("cleanStaleTasks")),
            clean_stale_tasks_description: shared(translator.text("cleanStaleTasksDesc")),
            delete_confirm_with_file: shared(translator.text("deleteConfirmDescWithFile")),
            delete_confirm_keep_file: shared(translator.text("deleteConfirmDescKeepFile")),
            batch_delete_confirm_with_file: shared(
                translator.text("batchDeleteConfirmDescWithFile"),
            ),
            too_many_windows_hint: shared(translator.text("tooManyWindowsHint")),
            main_queue: shared(translator.text(keys::MAIN_QUEUE)),
            pause: shared(translator.text(keys::PAUSE)),
            pause_all: shared(translator.text("pauseAll")),
            resume: shared(translator.text(keys::RESUME)),
            resume_all: shared(translator.text("resumeAll")),
            pause_all_unavailable: shared(translator.text("pauseAllUnavailable")),
            resume_all_unavailable: shared(translator.text("resumeAllUnavailable")),
            resume_all_queue_stopped: shared(translator.text("resumeAllQueueStopped")),
            open_file: shared(translator.text("openFile")),
            open_folder: shared(translator.text("openFolder")),
            copy_file: shared(translator.text("copyFileAction")),
            remote_tasks: shared(translator.text("remoteTasksGroup")),
            sidebar_queues: shared(translator.text(keys::SIDEBAR_QUEUES)),
            status_all: shared(translator.text(keys::TAB_ALL)),
            status_completed: shared(translator.text(keys::STATUS_COMPLETED)),
            status_downloading: shared(translator.text(keys::STATUS_DOWNLOADING)),
            status_error: shared(translator.text(keys::STATUS_ERROR)),
            task_error_post_only_endpoint: shared(translator.text("taskErrorPostOnlyEndpoint")),
            task_error_post_only_resume: shared(translator.text("taskErrorPostOnlyResume")),
            status_incomplete: shared(translator.text("tabDownloading")),
            status_paused: shared(translator.text(keys::STATUS_PAUSED)),
            status_seeding: shared(translator.text("statusSeeding")),
            seeding_status_queued: shared(translator.text("seedingStatusQueued")),
            status_pending: shared(translator.text("statusPending")),
            today: shared(translator.text(keys::TODAY)),
            yesterday: shared(translator.text("yesterday")),
            this_week: shared(translator.text("thisWeek")),
            this_month: shared(translator.text("thisMonth")),
            older: shared(translator.text("older")),
            site_bt: shared(translator.text("viewSiteBt")),
            ignore_plugin_retry: shared(translator.text("taskIgnorePluginRetry")),
            ignore_plugin_retry_title: shared(translator.text("taskIgnorePluginRetryTitle")),
            ignore_plugin_retry_msg: shared(translator.text("taskIgnorePluginRetryMsg")),
            boost_download: shared(translator.text("boostDownload")),
            cancel_boost: shared(translator.text("cancelBoost")),
            boost_task_completed: shared(translator.text("boostTaskCompleted")),
            boost_task_ended: shared(translator.text("boostTaskEnded")),
            rename_task: shared(translator.text("renameTask")),
            rename_task_title: shared(translator.text("renameTaskTitle")),
            rename_task_placeholder: shared(translator.text("renameTaskPlaceholder")),
            change_url: shared(translator.text("webChangeUrl")),
            change_url_title: shared(translator.text("webChangeUrlTitle")),
            change_url_label: shared(translator.text("webChangeUrlLabel")),
            change_url_placeholder: shared(translator.text("webChangeUrlPlaceholder")),
            edit_threads: shared(translator.text("editThreads")),
            edit_threads_title: shared(translator.text("editThreadsTitle")),
            edit_threads_hint: shared(translator.text("editThreadsResetHint")),
            edit_threads_label: shared(translator.text("configuredThreads")),
            edit_threads_zero_hint: shared(translator.text("queueDefaultSegmentsHint")),
            edit_threads_invalid: shared(translator.text("editThreadsInvalid")),
            threads_changed: shared(translator.text("threadsChanged")),
            seed_local_title: shared(translator.text("seedLocalTitle")),
            seed_local_desc: shared(translator.text("seedLocalDesc")),
            seed_local_path: shared(translator.text("seedLocalPath")),
            seed_local_path_placeholder: shared(translator.text("seedLocalPathPlaceholder")),
            seed_local_pick_file: shared(translator.text("seedLocalPickFile")),
            seed_local_pick_folder: shared(translator.text("seedLocalPickFolder")),
            seed_local_torrent: shared(translator.text("seedLocalTorrent")),
            seed_local_torrent_hint: shared(translator.text("seedLocalTorrentHint")),
            seed_local_pick_torrent: shared(translator.text("seedLocalPickTorrent")),
            seed_local_trackers: shared(translator.text("seedLocalTrackers")),
            seed_local_trackers_hint: shared(translator.text("seedLocalTrackersHint")),
            seed_local_unlimited: shared(translator.text("seedLocalUnlimited")),
            seed_local_unlimited_desc: shared(translator.text("seedLocalUnlimitedDesc")),
            seed_local_submit: shared(translator.text("seedLocalSubmit")),
            seed_local_working: shared(translator.text("seedLocalWorking")),
            seed_local_started: shared(translator.text("seedLocalStarted")),
            seed_local_err_path_required: shared(translator.text("seedLocalErrPathRequired")),
            verify_integrity: shared(translator.text("verifyTaskIntegrity")),
            redownload_task: shared(translator.text("redownloadTask")),
            copy_url: shared(translator.text("copyUrl")),
            copy_magnet: shared(translator.text("btCopyMagnet")),
            export_torrent: shared(translator.text("btExportTorrent")),
            url_copied: shared(translator.text("urlCopied")),
            move_to_queue: shared(translator.text("moveToQueueAction")),
            select_tasks: shared(translator.text("selectTasks")),
            select_all_tasks: shared(translator.text("selectAll")),
            select_task_tooltip: shared(translator.text("selectTaskTooltip")),
            detail: shared(translator.text("detail")),
            open_in_window: shared(translator.text("menuOpenInWindow")),
            group_pause_all: shared(translator.text("groupPauseAll")),
            group_resume_all: shared(translator.text("groupResumeAll")),
            group_retry_failed: shared(translator.text("groupRetryFailed")),
            group_open_folder: shared(translator.text("groupOpenFolder")),
            group_copy_source_link: shared(translator.text("groupCopySourceLink")),
            group_delete: shared(translator.text("groupDelete")),
            group_delete_with_files: shared(translator.text("groupDeleteWithFiles")),
            group_delete_confirm_with_file: shared(translator.text("deleteConfirmDescWithFile")),
            open_group_in_window: shared(translator.text("openGroupInWindowAction")),
            group_items_count: shared(translator.text("groupItemsCount")),
            group_done_count: shared(translator.text("groupDoneCount")),
            group_downloading_count: shared(translator.text("groupDownloadingCount")),
            group_pending_count: shared(translator.text("groupPendingCount")),
            group_paused_count: shared(translator.text("groupPausedCount")),
            group_failed_count: shared(translator.text("groupFailedCount")),
            group_done_of_total: shared(translator.text("groupDoneOfTotal")),
            group_eta_remaining: shared(translator.text("groupEtaRemaining")),
            group_dir_count: shared(translator.text("groupDirCount")),
            group_detail_subtitle: shared(translator.text("groupDetailSubtitle")),
            group_detail_source: shared(translator.text("groupDetailSource")),
            group_detail_save_dir: shared(translator.text("groupDetailSaveDir")),
            group_detail_created_at: shared(translator.text("groupDetailCreatedAt")),
            group_detail_queue: shared(translator.text("groupDetailQueue")),
            search_tasks_placeholder: shared(translator.text("searchTasksPlaceholder")),
            unsupported_drop_hint: shared(translator.text("unsupportedDropHint")),
            sidebar_status: shared(translator.text(keys::SIDEBAR_STATUS)),
            sidebar_devices: shared(translator.text("deviceSection")),
            device_download_to_this: shared(translator.text("deviceDownloadToThis")),
            linked_device_rename: shared(translator.text("linkedDeviceRename")),
            linked_device_unpair: shared(translator.text("localDeviceUnpair")),
            device_presence_unknown: shared(translator.text("devicePresenceUnknown")),
            this_device: shared(translator.text("thisDevice")),
            all_devices: shared(translator.text("allDevices")),
            add_device: shared(translator.text("addDeviceEntry")),
            status_canceled: shared(translator.text("statusCanceled")),
            status_file_missing: shared(translator.text("statusFileMissing")),
            conflict_pending: shared(translator.text("fileConflictPending")),
            conflict_pending_tooltip: shared(translator.text("fileConflictPendingTooltip")),
            add_category: shared(translator.text("addCategory")),
            edit_category: shared(translator.text("editCategory")),
            hide_section: shared(translator.text("hideSection")),
            start_queue_action: shared(translator.text("startQueueAction")),
            stop_queue_action: shared(translator.text("stopQueueAction")),
            manage_queue_action: shared(translator.text("manageQueueAction")),
            delete_queue_action: shared(translator.text("deleteQueueAction")),
            queue_delete_confirm_desc: shared(translator.text("queueDeleteConfirmDesc")),
            tab_failed: shared(translator.text("tabError")),
        }
    }

    pub(crate) fn format_eta(&self, seconds: u64) -> SharedString {
        let (template, value) = if seconds < 60 {
            (&self.eta_seconds, seconds.to_string())
        } else if seconds < 3600 {
            (&self.eta_minutes, (seconds / 60).to_string())
        } else {
            (&self.eta_hours, format!("{:.1}", seconds as f64 / 3600.0))
        };
        SharedString::from(template.replace("{n}", &value))
    }

    /// 「删除任务及文件」确认文案：单个带文件名，多个带数量。
    pub(crate) fn delete_with_files_description(
        &self,
        keys: &[crate::model::RowKey],
        store: &crate::model::TaskStore,
    ) -> SharedString {
        if let [key] = keys {
            let name = store
                .get(key)
                .map(|row| row.name.clone())
                .unwrap_or_default();
            SharedString::from(self.delete_confirm_with_file.replace("{fileName}", &name))
        } else {
            SharedString::from(
                self.batch_delete_confirm_with_file
                    .replace("{count}", &keys.len().to_string()),
            )
        }
    }

    /// 单任务删除确认文案：按是否同时删除文件选模板，代入任务名。
    pub(crate) fn delete_task_description(&self, name: &str, delete_files: bool) -> SharedString {
        let template = if delete_files {
            &self.delete_confirm_with_file
        } else {
            &self.delete_confirm_keep_file
        };
        SharedString::from(template.replace("{fileName}", name))
    }

    /// 「删除任务组及文件」确认文案：复用单任务模板，代入组名。
    pub(crate) fn group_delete_with_files_description(&self, group_name: &str) -> SharedString {
        SharedString::from(
            self.group_delete_confirm_with_file
                .replace("{fileName}", group_name),
        )
    }

    /// 组计数行各段（调用方以 ` · ` 连接）：总数恒在，其余为 0 的段省略；剩余时间只在
    /// 一天以内时给出。
    pub(crate) fn group_count_segments(
        &self,
        counts: &crate::model::group_tree::GroupCounts,
        eta_seconds: Option<u64>,
    ) -> Vec<(GroupCountTone, SharedString)> {
        let count = |template: &SharedString, value: usize| {
            SharedString::from(template.replace("{n}", &value.to_string()))
        };
        let mut segments = vec![(
            GroupCountTone::Plain,
            count(&self.group_items_count, counts.total),
        )];
        for (value, template, tone) in [
            (counts.done, &self.group_done_count, GroupCountTone::Plain),
            (
                counts.downloading,
                &self.group_downloading_count,
                GroupCountTone::Active,
            ),
            (
                counts.pending,
                &self.group_pending_count,
                GroupCountTone::Plain,
            ),
            (
                counts.paused,
                &self.group_paused_count,
                GroupCountTone::Plain,
            ),
            (
                counts.failed,
                &self.group_failed_count,
                GroupCountTone::Failed,
            ),
        ] {
            if value > 0 {
                segments.push((tone, count(template, value)));
            }
        }
        if let Some(eta) = eta_seconds.filter(|seconds| *seconds <= 86_400) {
            segments.push((
                GroupCountTone::Plain,
                SharedString::from(
                    self.group_eta_remaining
                        .replace("{eta}", &self.format_eta(eta)),
                ),
            ));
        }
        segments
    }

    /// `{done}/{total}`。
    pub(crate) fn format_group_done_of_total(&self, done: usize, total: usize) -> SharedString {
        SharedString::from(
            self.group_done_of_total
                .replace("{done}", &done.to_string())
                .replace("{total}", &total.to_string()),
        )
    }

    /// 目录行文件数：`{count} 个文件`（目录大小在「大小」列）。
    pub(crate) fn format_group_dir_count(&self, count: usize) -> SharedString {
        SharedString::from(self.group_dir_count.replace("{count}", &count.to_string()))
    }

    /// 组概览副标题：`任务组 · {status}`。
    pub(crate) fn format_group_subtitle(&self, status: &str) -> SharedString {
        SharedString::from(self.group_detail_subtitle.replace("{status}", status))
    }

    pub(crate) fn state_label(&self, state: crate::model::TaskState) -> SharedString {
        match state {
            crate::model::TaskState::Pending => self.status_pending.clone(),
            crate::model::TaskState::Downloading => self.status_downloading.clone(),
            crate::model::TaskState::Paused => self.status_paused.clone(),
            crate::model::TaskState::Completed => self.status_completed.clone(),
            crate::model::TaskState::Failed => self.status_error.clone(),
        }
    }

    /// 行级状态名：已完成但产物已不在下载目录时显示「文件已删除」，引擎 status 5（探测 /
    /// 插件解析）显示「准备中」。所有按任务展示状态名的地方都走这里，不得直接 `state_label`。
    pub(crate) fn task_state_label(&self, task: &crate::model::DownloadTaskView) -> SharedString {
        if task.is_file_missing() {
            self.status_file_missing.clone()
        } else if task.state == crate::model::TaskState::Pending && task.preparing {
            self.metadata_loading.clone()
        } else if task.is_seeding() {
            // seeding_status 8 = 排队等待做种名额；1 = 做种中。
            if task.seeding_status == 8 {
                self.seeding_status_queued.clone()
            } else {
                self.status_seeding.clone()
            }
        } else {
            self.state_label(task.state)
        }
    }

    /// 单行状态（详情窗口状态行、进度窗口标题下）：排队中显示「排队 #n」，其余同
    /// [`Self::task_state_label`]。
    pub(crate) fn task_status_text(
        &self,
        task: &crate::model::DownloadTaskView,
        store: &crate::model::TaskStore,
    ) -> SharedString {
        self.queued_label(task, store)
            .map_or_else(|| self.task_state_label(task), SharedString::from)
    }

    /// 只有引擎原始 status 的场景（活动日志的状态变更）：与 [`Self::task_state_label`]
    /// 同一规则，5 为「准备中」而非「等待中」。
    pub(crate) fn status_code_label(&self, status: i32) -> SharedString {
        if status == 5 {
            self.metadata_loading.clone()
        } else {
            self.state_label(crate::model::TaskState::from_status(status))
        }
    }

    /// 排队中（引擎待启动队列内、非准备中）的「排队 #n」；其余任务为 `None`。序号是行排序键
    /// 在 `store` 排队键集合中的名次，渲染时二分得到。
    pub(crate) fn queued_label(
        &self,
        task: &crate::model::DownloadTaskView,
        store: &crate::model::TaskStore,
    ) -> Option<String> {
        if task.state != crate::model::TaskState::Pending || task.preparing {
            return None;
        }
        let rank = store.queue_rank(task);
        (rank > 0).then(|| self.subtitle_queued.replace("{pos}", &rank.to_string()))
    }

    pub(crate) fn date_bucket_label(
        &self,
        bucket: crate::model::view_prefs::DateBucket,
    ) -> SharedString {
        use crate::model::view_prefs::DateBucket;
        match bucket {
            DateBucket::Today => self.today.clone(),
            DateBucket::Yesterday => self.yesterday.clone(),
            DateBucket::ThisWeek => self.this_week.clone(),
            DateBucket::ThisMonth => self.this_month.clone(),
            DateBucket::Older => self.older.clone(),
        }
    }

    /// 分类显示名：内置项走 i18n（`categoryVideo`…），自定义项用 `name`。
    pub(crate) fn category_label(
        &self,
        dto: &fluxdown_protocol::CustomCategoryDto,
    ) -> SharedString {
        match dto.builtin_type.as_deref() {
            Some("all") => self.status_all.clone(),
            Some("video") => self.category_video.clone(),
            Some("audio") => self.category_audio.clone(),
            Some("document") => self.category_document.clone(),
            Some("image") => self.category_image.clone(),
            Some("program") => self.category_program.clone(),
            Some("archive") => self.category_archive.clone(),
            Some("other") => self.category_other.clone(),
            _ => SharedString::from(dto.name.clone()),
        }
    }

    /// 队列显示名：内置队列本地化，自定义队列用用户命名。
    pub(crate) fn queue_name(&self, queue_id: &str, name: &str) -> SharedString {
        match queue_id {
            fluxdown_protocol::MAIN_QUEUE_ID => self.main_queue.clone(),
            fluxdown_protocol::LATER_QUEUE_ID => self.later_queue.clone(),
            _ => SharedString::from(name.to_owned()),
        }
    }

    /// 无 host 任务的站点标签：BT 用「BT · Magnet」，其余 `—`。
    pub(crate) fn site_unknown(&self, task: &crate::model::DownloadTaskView) -> SharedString {
        if task.protocol == crate::model::TaskProtocol::Bt {
            self.site_bt.clone()
        } else {
            SharedString::from("—")
        }
    }

    /// 任务表载入态说明：`共 {total} 个任务，…`。
    pub(crate) fn format_tasks_loading_subtitle(&self, total: usize) -> SharedString {
        SharedString::from(
            self.tasks_loading_subtitle
                .replace("{total}", &group_digits(total)),
        )
    }

    /// 导入进度：`正在添加 {done} / {total}`。
    pub(crate) fn format_import_progress(&self, done: usize, total: usize) -> SharedString {
        SharedString::from(
            self.import_progress
                .replace("{done}", &group_digits(done))
                .replace("{total}", &group_digits(total)),
        )
    }

    /// 导入重试等待：`连接中断，{seconds} 秒后重试（{attempt}/{max}）`。
    pub(crate) fn format_import_retrying(
        &self,
        seconds: u64,
        attempt: u32,
        max: u32,
    ) -> SharedString {
        SharedString::from(
            self.import_retrying
                .replace("{seconds}", &seconds.to_string())
                .replace("{attempt}", &attempt.to_string())
                .replace("{max}", &max.to_string()),
        )
    }

    /// 删除进度：`正在删除 {done} / {total}`。
    pub(crate) fn format_delete_progress(&self, done: usize, total: usize) -> SharedString {
        SharedString::from(
            self.delete_progress
                .replace("{done}", &group_digits(done))
                .replace("{total}", &group_digits(total)),
        )
    }

    /// 删除完成提示：`已删除 {n} 个任务`（与移动端共用 `mobileToastDeletedN`）。
    pub(crate) fn format_deleted_count(&self, count: usize) -> String {
        self.deleted_count.replace("{n}", &group_digits(count))
    }

    /// 已取消删除：`已取消删除：已删除 {done} / 共 {total}`。
    pub(crate) fn format_delete_cancelled(&self, done: usize, total: usize) -> String {
        self.delete_cancelled
            .replace("{done}", &group_digits(done))
            .replace("{total}", &group_digits(total))
    }

    /// 部分删除：`已删除 {done} / 共 {total}，{failed} 个失败`。
    pub(crate) fn format_delete_partial(&self, done: usize, total: usize, failed: usize) -> String {
        self.delete_partial
            .replace("{done}", &group_digits(done))
            .replace("{total}", &group_digits(total))
            .replace("{failed}", &group_digits(failed))
    }

    /// 完整本地时间 `YYYY-MM-DD HH:MM:SS`（Unix 秒）：列表「创建时间」列与任务详情共用。
    pub(crate) fn format_datetime(timestamp_secs: i64) -> SharedString {
        use chrono::{Local, TimeZone};
        Local
            .timestamp_opt(timestamp_secs, 0)
            .single()
            .map(|at| SharedString::from(at.format("%Y-%m-%d %H:%M:%S").to_string()))
            .unwrap_or_else(|| SharedString::from("—"))
    }
}

/// 「新建下载」表单文案，使用公共 i18n 资源键。
#[derive(Clone)]
pub(crate) struct NewDownloadStrings {
    pub(crate) subtitle: SharedString,
    pub(crate) url_label: SharedString,
    pub(crate) url_placeholder: SharedString,
    url_count: SharedString,
    pub(crate) probe_checking: SharedString,
    probe_found: SharedString,
    probe_size_unknown: SharedString,
    pub(crate) probe_unavailable: SharedString,
    pub(crate) no_valid_url: SharedString,
    pub(crate) open_torrent: SharedString,
    pub(crate) select_torrent: SharedString,
    pub(crate) import_txt: SharedString,
    pub(crate) import_txt_none: SharedString,
    import_txt_found: SharedString,
    pub(crate) save_dir: SharedString,
    pub(crate) save_dir_placeholder: SharedString,
    pub(crate) browse: SharedString,
    pub(crate) category_auto: SharedString,
    category_auto_matched: SharedString,
    pub(crate) category_auto_desc: SharedString,
    pub(crate) category_none: SharedString,
    pub(crate) category_none_desc: SharedString,
    /// 内置分类（`builtin_type` → 本地化显示名）。
    category_builtin_names: Vec<(&'static str, SharedString)>,
    pub(crate) threads: SharedString,
    pub(crate) threads_auto: SharedString,
    pub(crate) threads_custom: SharedString,
    pub(crate) threads_custom_hint: SharedString,
    pub(crate) rename: SharedString,
    pub(crate) rename_placeholder: SharedString,
    pub(crate) advanced: SharedString,
    pub(crate) http_auth: SharedString,
    pub(crate) http_auth_desc: SharedString,
    pub(crate) http_auth_user: SharedString,
    pub(crate) http_auth_password: SharedString,
    pub(crate) http_auth_save: SharedString,
    pub(crate) capture_auth_hint: SharedString,
    pub(crate) capture_context_hint: SharedString,
    pub(crate) proxy: SharedString,
    pub(crate) proxy_desc: SharedString,
    pub(crate) proxy_placeholder: SharedString,
    pub(crate) proxy_follow: SharedString,
    pub(crate) proxy_direct: SharedString,
    pub(crate) proxy_system: SharedString,
    pub(crate) proxy_global_manual: SharedString,
    pub(crate) proxy_custom: SharedString,
    pub(crate) proxy_not_configured: SharedString,
    pub(crate) ignore_tls: SharedString,
    pub(crate) ignore_tls_desc: SharedString,
    pub(crate) user_agent: SharedString,
    pub(crate) user_agent_desc: SharedString,
    pub(crate) ua_inherit: SharedString,
    pub(crate) ua_chrome: SharedString,
    pub(crate) ua_firefox: SharedString,
    pub(crate) ua_edge: SharedString,
    pub(crate) ua_safari: SharedString,
    pub(crate) ua_custom: SharedString,
    pub(crate) cookie: SharedString,
    pub(crate) cookie_desc: SharedString,
    pub(crate) cookie_placeholder: SharedString,
    pub(crate) checksum: SharedString,
    pub(crate) checksum_desc: SharedString,
    pub(crate) checksum_placeholder: SharedString,
    pub(crate) headers: SharedString,
    pub(crate) headers_desc: SharedString,
    pub(crate) header_name: SharedString,
    pub(crate) header_value: SharedString,
    pub(crate) add_header: SharedString,
    pub(crate) cancel: SharedString,
    pub(crate) download_later: SharedString,
    pub(crate) start_download: SharedString,
    start_batch: SharedString,
    later_tooltip: SharedString,
    start_tooltip: SharedString,
    pub(crate) main_queue: SharedString,
    pub(crate) later_queue: SharedString,
    pub(crate) download_to: SharedString,
    pub(crate) download_to_hint: SharedString,
    pub(crate) this_device: SharedString,
    pub(crate) device_online: SharedString,
    pub(crate) device_offline: SharedString,
    pub(crate) device_presence_unknown: SharedString,
    pub(crate) device_lan_tag: SharedString,
    pub(crate) target_offline_hint: SharedString,
    pub(crate) target_paired_offline_hint: SharedString,
    pub(crate) remote_options_hint: SharedString,
    pub(crate) remote_dir_hint: SharedString,
    remote_dir_default: SharedString,
    pub(crate) remote_dir_use_default: SharedString,
    path_invalid: SharedString,
    dispatched: SharedString,
    dispatched_offline: SharedString,
    link_dispatched: SharedString,
    dispatch_partial: SharedString,
    pub(crate) dispatch_failed: SharedString,
    import_cancelled: SharedString,
    import_partial: SharedString,
}

impl NewDownloadStrings {
    pub(crate) fn from_translator(translator: &Translator) -> Self {
        Self {
            subtitle: shared(translator.text("batchDownloadDesc")),
            url_label: shared(translator.text("downloadUrl")),
            url_placeholder: shared(translator.text("newDownloadUrlPlaceholder")),
            url_count: shared(translator.text("urlCount")),
            probe_checking: shared(translator.text("newDownloadProbeChecking")),
            probe_found: shared(translator.text("newDownloadProbeFound")),
            probe_size_unknown: shared(translator.text("newDownloadProbeSizeUnknown")),
            probe_unavailable: shared(translator.text("newDownloadProbeUnavailable")),
            no_valid_url: shared(translator.text("newDownloadNoValidUrl")),
            open_torrent: shared(translator.text("openTorrentFile")),
            select_torrent: shared(translator.text("selectTorrentFile")),
            import_txt: shared(translator.text("importTxtFile")),
            import_txt_none: shared(translator.text("importTxtNoUrls")),
            import_txt_found: shared(translator.text("importTxtFound")),
            save_dir: shared(translator.text("saveDir")),
            save_dir_placeholder: shared(translator.text("selectSaveDir")),
            browse: shared(translator.text("browse")),
            category_auto: shared(translator.text("saveDirCategoryAuto")),
            category_auto_matched: shared(translator.text("saveDirCategoryAutoMatched")),
            category_auto_desc: shared(translator.text("saveDirCategoryAutoDesc")),
            category_none: shared(translator.text("saveDirCategoryNone")),
            category_none_desc: shared(translator.text("saveDirCategoryNoneDesc")),
            category_builtin_names: [
                ("video", "categoryVideo"),
                ("audio", "categoryAudio"),
                ("document", "categoryDocument"),
                ("image", "categoryImage"),
                ("program", "categoryProgram"),
                ("archive", "categoryArchive"),
                ("other", "categoryOther"),
            ]
            .into_iter()
            .map(|(kind, key)| (kind, shared(translator.text(key))))
            .collect(),
            threads: shared(translator.text("threads")),
            threads_auto: shared(translator.text("auto")),
            threads_custom: shared(translator.text("customThreads")),
            threads_custom_hint: shared(&translator.text_with(
                "customRangeHint",
                &[
                    ("min", "1"),
                    ("max", &crate::model::new_download::MAX_THREADS.to_string()),
                ],
            )),
            rename: shared(translator.text("renameOptional")),
            rename_placeholder: shared(translator.text("autoDetectFilename")),
            advanced: shared(translator.text("taskProxyAdvanced")),
            http_auth: shared(translator.text("taskHttpAuth")),
            http_auth_desc: shared(translator.text("taskHttpAuthDesc")),
            http_auth_user: shared(translator.text("taskHttpAuthUser")),
            http_auth_password: shared(translator.text("taskHttpAuthPassword")),
            http_auth_save: shared(translator.text("taskHttpAuthSaveForSite")),
            capture_auth_hint: shared(translator.text("newDownloadCaptureAuthHint")),
            capture_context_hint: shared(translator.text("newDownloadCaptureContextHint")),
            proxy: shared(translator.text("taskProxy")),
            proxy_desc: shared(translator.text("taskProxyDesc")),
            proxy_placeholder: shared(translator.text("taskProxyPlaceholder")),
            proxy_follow: shared(translator.text("taskProxyChoiceFollow")),
            proxy_direct: shared(translator.text("taskProxyChoiceDirect")),
            proxy_system: shared(translator.text("taskProxyChoiceSystem")),
            proxy_global_manual: shared(translator.text("taskProxyChoiceGlobalManual")),
            proxy_custom: shared(translator.text("taskProxyChoiceCustom")),
            proxy_not_configured: shared(translator.text("proxyNotConfigured")),
            ignore_tls: shared(translator.text("taskIgnoreTlsErrors")),
            ignore_tls_desc: shared(translator.text("taskIgnoreTlsErrorsDesc")),
            user_agent: shared(translator.text("userAgent")),
            user_agent_desc: shared(translator.text("userAgentTaskPlaceholder")),
            ua_inherit: shared(translator.text("queueUaInheritGlobal")),
            ua_chrome: shared(translator.text("userAgentPresetChrome")),
            ua_firefox: shared(translator.text("userAgentPresetFirefox")),
            ua_edge: shared(translator.text("userAgentPresetEdge")),
            ua_safari: shared(translator.text("userAgentPresetSafari")),
            ua_custom: shared(translator.text("userAgentPresetCustom")),
            cookie: shared(translator.text("taskCookie")),
            cookie_desc: shared(translator.text("taskCookieDesc")),
            cookie_placeholder: shared(translator.text("taskCookiePlaceholder")),
            checksum: shared(translator.text("taskChecksum")),
            checksum_desc: shared(translator.text("taskChecksumDesc")),
            checksum_placeholder: shared(translator.text("taskChecksumPlaceholder")),
            headers: shared(translator.text("taskHeaders")),
            headers_desc: shared(translator.text("taskHeadersDesc")),
            header_name: shared(translator.text("taskHeadersKeyPlaceholder")),
            header_value: shared(translator.text("taskHeadersValuePlaceholder")),
            add_header: shared(translator.text("taskHeadersAdd")),
            cancel: shared(translator.text("cancel")),
            download_later: shared(translator.text("downloadLater")),
            start_download: shared(translator.text("startDownload")),
            start_batch: shared(translator.text("startBatchDownload")),
            later_tooltip: shared(translator.text("laterIntoQueueTooltip")),
            start_tooltip: shared(translator.text("startIntoQueueTooltip")),
            main_queue: shared(translator.text(keys::MAIN_QUEUE)),
            later_queue: shared(translator.text(keys::LATER_QUEUE)),
            download_to: shared(translator.text("downloadTo")),
            download_to_hint: shared(translator.text("downloadToHint")),
            this_device: shared(translator.text("thisDevice")),
            device_online: shared(translator.text("deviceOnline")),
            device_offline: shared(translator.text("deviceOffline")),
            device_presence_unknown: shared(translator.text("devicePresenceUnknown")),
            device_lan_tag: shared(translator.text("deviceLocalTag")),
            target_offline_hint: shared(translator.text("downloadToOfflineHint")),
            target_paired_offline_hint: shared(translator.text("errReasonPeerOffline")),
            remote_options_hint: shared(translator.text("downloadToRemoteOptionsIgnored")),
            remote_dir_hint: shared(translator.text("downloadToRemoteDirHint")),
            remote_dir_default: shared(translator.text("downloadToRemoteDirDefault")),
            remote_dir_use_default: shared(translator.text("downloadToRemoteDirUseDefault")),
            path_invalid: shared(translator.text("downloadToPathInvalid")),
            dispatched: shared(translator.text("downloadToDispatched")),
            dispatched_offline: shared(translator.text("downloadToDispatchedOffline")),
            link_dispatched: shared(translator.text("linkDispatchedToDevice")),
            dispatch_partial: shared(translator.text("downloadToPartial")),
            dispatch_failed: shared(translator.text("dispatchFailed")),
            import_cancelled: shared(translator.text("importCancelled")),
            import_partial: shared(translator.text("importPartial")),
        }
    }

    /// `{count} 个链接`。
    pub(crate) fn format_url_count(&self, count: usize) -> SharedString {
        SharedString::from(self.url_count.replace("{count}", &count.to_string()))
    }

    /// 分类显示名：内置项走 i18n，自定义项用 `name`。
    pub(crate) fn category_name(
        &self,
        category: &crate::model::save_dir_category::NewDownloadCategory,
    ) -> SharedString {
        category
            .builtin_type
            .as_deref()
            .and_then(|kind| {
                self.category_builtin_names
                    .iter()
                    .find(|(builtin, _)| *builtin == kind)
            })
            .map_or_else(
                || SharedString::from(category.name.clone()),
                |(_, label)| label.clone(),
            )
    }

    /// 自动模式命中分类时的触发器文案：`自动 · {name}`。
    pub(crate) fn format_category_auto_matched(&self, name: &str) -> SharedString {
        SharedString::from(self.category_auto_matched.replace("{name}", name))
    }

    /// `已识别：{文件名 · 大小}`；大小未知（0）时显示「大小未知」。名称为空时只显示大小。
    pub(crate) fn format_probe_found(&self, file_name: &str, total_bytes: i64) -> SharedString {
        let size = match u64::try_from(total_bytes) {
            Ok(bytes) if bytes > 0 => crate::model::format_bytes(bytes),
            _ => self.probe_size_unknown.to_string(),
        };
        let info = if file_name.is_empty() {
            size
        } else {
            format!("{file_name} · {size}")
        };
        SharedString::from(self.probe_found.replace("{info}", &info))
    }

    /// `已导入 {count} 个链接`。
    pub(crate) fn format_import_found(&self, count: usize) -> SharedString {
        SharedString::from(self.import_txt_found.replace("{count}", &count.to_string()))
    }

    /// 开始按钮标签：单条 = `开始下载`，多条 = `下载 {count} 个文件`。
    pub(crate) fn format_start(&self, count: usize) -> SharedString {
        if count > 1 {
            SharedString::from(self.start_batch.replace("{count}", &count.to_string()))
        } else {
            self.start_download.clone()
        }
    }

    /// 「稍后下载」按钮提示：`创建任务但不开始，加入「{name}」…`。
    pub(crate) fn format_later_tooltip(&self, queue_name: &str) -> SharedString {
        SharedString::from(self.later_tooltip.replace("{name}", queue_name))
    }

    /// 「开始下载」按钮提示：`下载到「{name}」…`。
    pub(crate) fn format_start_tooltip(&self, queue_name: &str) -> SharedString {
        SharedString::from(self.start_tooltip.replace("{name}", queue_name))
    }

    /// 队列显示名：内置队列本地化，自定义队列用用户命名。
    pub(crate) fn queue_name(&self, queue_id: &str, name: &str) -> SharedString {
        match queue_id {
            fluxdown_protocol::MAIN_QUEUE_ID => self.main_queue.clone(),
            fluxdown_protocol::LATER_QUEUE_ID => self.later_queue.clone(),
            _ => shared(name),
        }
    }

    /// 远端保存目录输入框占位：目标设备自报的默认目录（无则「目标设备默认目录」）。
    pub(crate) fn remote_dir_placeholder(&self, default_dir: Option<&str>) -> SharedString {
        match default_dir {
            Some(dir) => SharedString::from(self.remote_dir_default.replace("{dir}", dir)),
            None => self.remote_dir_use_default.clone(),
        }
    }

    /// 保存目录不是目标路径风格的绝对路径时的错误提示（带该风格的示例）。
    pub(crate) fn path_invalid_text(
        &self,
        style: Option<fluxdown_protocol::PathStyle>,
    ) -> SharedString {
        let example = match style {
            Some(fluxdown_protocol::PathStyle::Windows) => r"C:\Downloads",
            _ => "/home/user/Downloads",
        };
        SharedString::from(self.path_invalid.replace("{example}", example))
    }

    /// 下发成功提示；目标离线（云设备）时说明上线后才执行；直连下发是一次性投递，
    /// 提示进度要在目标设备上查看。
    pub(crate) fn format_dispatched(
        &self,
        count: usize,
        device: &str,
        offline: bool,
        direct: bool,
    ) -> String {
        if direct {
            return self.link_dispatched.replace("{device}", device);
        }
        let template = if offline {
            &self.dispatched_offline
        } else {
            &self.dispatched
        };
        template
            .replace("{count}", &count.to_string())
            .replace("{device}", device)
    }

    /// 部分成功 / 部分失败提示。
    pub(crate) fn format_partial(&self, ok: usize, failed: usize, device: &str) -> String {
        self.dispatch_partial
            .replace("{ok}", &ok.to_string())
            .replace("{failed}", &failed.to_string())
            .replace("{device}", device)
    }

    /// 已取消导入：`已取消导入：已添加 {added} / 共 {total}`。
    pub(crate) fn format_import_cancelled(&self, added: usize, total: usize) -> String {
        self.import_cancelled
            .replace("{added}", &group_digits(added))
            .replace("{total}", &group_digits(total))
    }

    /// 部分添加：`已添加 {added} / 共 {total}；{failed} 个失败`。
    pub(crate) fn format_import_partial(
        &self,
        added: usize,
        total: usize,
        failed: usize,
    ) -> String {
        self.import_partial
            .replace("{added}", &group_digits(added))
            .replace("{total}", &group_digits(total))
            .replace("{failed}", &group_digits(failed))
    }
}

/// agent 端口不透传服务端 message：优先按 `reason` 给出可操作文案，缺失再按码回退。
pub(crate) fn error_text(
    translator: &Translator,
    error: &fluxdown_protocol::RpcErrorData,
) -> String {
    use fluxdown_protocol::{ApplicationErrorCode, ErrorReason};
    let reason_key = error.reason.and_then(|reason| match reason {
        ErrorReason::TargetDeviceOffline => Some("errReasonTargetDeviceOffline"),
        ErrorReason::TaskStateConflict => Some("errReasonTaskStateConflict"),
        ErrorReason::TaskDeviceMismatch => Some("errReasonTaskDeviceMismatch"),
        ErrorReason::SaveDirUnavailable => Some("errReasonSaveDirUnavailable"),
        ErrorReason::CloudUnreachable => Some("accountErrorNetwork"),
        ErrorReason::SessionExpired => Some("errReasonSessionExpired"),
        ErrorReason::DeviceUntrusted => Some("accountSessionRevokedUntrusted"),
        ErrorReason::PasswordChanged => Some("accountSessionRevokedPasswordChanged"),
        ErrorReason::PeerNotPaired => Some("errReasonPeerNotPaired"),
        ErrorReason::PeerOffline => Some("errReasonPeerOffline"),
        ErrorReason::PeerOutdated => Some("errReasonPeerOutdated"),
        ErrorReason::LinkUnavailable => Some("errReasonLinkUnavailable"),
        ErrorReason::PeerClockSkew => Some("errReasonPeerClockSkew"),
        _ => None,
    });
    let key = reason_key.unwrap_or(match error.code {
        ApplicationErrorCode::Unavailable | ApplicationErrorCode::Timeout => {
            "localServiceDisconnected"
        }
        ApplicationErrorCode::InvalidArgument | ApplicationErrorCode::NotFound => {
            "localServiceInvalidArgument"
        }
        ApplicationErrorCode::Conflict => "localServiceConflict",
        ApplicationErrorCode::Unsupported => "settingsUnsupportedOnPlatform",
        ApplicationErrorCode::ProtocolIncompatible
        | ApplicationErrorCode::Unauthorized
        | ApplicationErrorCode::Cancelled
        | ApplicationErrorCode::Internal => "localServiceActionFailed",
    });
    translator.text(key).to_owned()
}

/// 更换链接失败文案：竞态等 RPC 失败走通用文案；`not-found`（任务已不存在）用专用文案。
/// 输入类错误由对话框预校验拦截，不会到这里。
pub(crate) fn change_url_error_text(
    translator: &Translator,
    error: &fluxdown_protocol::RpcErrorData,
) -> String {
    if error.code == fluxdown_protocol::ApplicationErrorCode::NotFound {
        translator.text("webChangeUrlErrNotFound").to_owned()
    } else {
        error_text(translator, error)
    }
}

/// 修改线程数失败文案：daemon 对「已完成 / 不存在」的任务返回 `not-found`。
pub(crate) fn set_segments_error_text(
    translator: &Translator,
    error: &fluxdown_protocol::RpcErrorData,
) -> String {
    if error.code == fluxdown_protocol::ApplicationErrorCode::NotFound {
        translator.text("threadsChangeRejected").to_owned()
    } else {
        error_text(translator, error)
    }
}

/// 千分位分组（`1234567` → `1,234,567`），进度与计数提示共用。
fn group_digits(value: usize) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

impl DownloadStrings {
    /// 任务错误消息的显示文本：引擎两条稳定错误整串匹配（trim 后）时返回本地化文案，
    /// 其余原样返回。仅用于显示 / 复制，逻辑判断仍用原始 `error_message`。
    pub(crate) fn task_error_text<'a>(&'a self, raw: &'a str) -> &'a str {
        match raw.trim() {
            POST_ONLY_ENDPOINT_MESSAGE => &self.task_error_post_only_endpoint,
            POST_ONLY_RESUME_MESSAGE => &self.task_error_post_only_resume,
            _ => raw,
        }
    }
}

/// 镜像引擎 `native/engine/src/downloader.rs` 的 `POST_ONLY_ENDPOINT_MESSAGE`；改动须同步。
const POST_ONLY_ENDPOINT_MESSAGE: &str =
    "This link only accepts POST requests (HTTP 405); start the download from the browser";
/// 镜像引擎 `native/engine/src/downloader.rs` 的 `POST_ONLY_RESUME_MESSAGE`；改动须同步。
const POST_ONLY_RESUME_MESSAGE: &str = "This task was started by a browser POST request and cannot be resumed; start it again from the browser";

fn shared(value: &str) -> SharedString {
    SharedString::from(value.to_owned())
}

#[cfg(test)]
mod tests {
    use chrono::{Local, TimeZone};

    use super::DownloadStrings;

    #[test]
    fn datetime_keeps_seconds_across_midnight_and_rejects_out_of_range() {
        let before = Local
            .with_ymd_and_hms(2026, 3, 18, 23, 59, 59)
            .single()
            .expect("valid local time");
        let after = Local
            .with_ymd_and_hms(2026, 3, 19, 0, 0, 0)
            .single()
            .expect("valid local time");
        assert_eq!(
            DownloadStrings::format_datetime(before.timestamp()).as_ref(),
            "2026-03-18 23:59:59"
        );
        assert_eq!(
            DownloadStrings::format_datetime(after.timestamp()).as_ref(),
            "2026-03-19 00:00:00"
        );
        assert_eq!(DownloadStrings::format_datetime(i64::MAX).as_ref(), "—");
    }

    #[test]
    fn digit_grouping_uses_thousands_separators() {
        use super::group_digits;
        for (value, expected) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1,000"),
            (1_234, "1,234"),
            (10_000, "10,000"),
            (1_234_567, "1,234,567"),
        ] {
            assert_eq!(group_digits(value), expected);
        }
    }

    #[test]
    fn post_only_errors_are_localized_and_others_pass_through()
    -> Result<(), fluxdown_ui_i18n::I18nError> {
        let catalog = std::sync::Arc::new(fluxdown_ui_i18n::I18nCatalog::load_embedded()?);
        let translator = catalog.translator("en");
        let strings = DownloadStrings::from_translator(&translator);
        assert_eq!(
            strings.task_error_text(super::POST_ONLY_ENDPOINT_MESSAGE),
            translator.text("taskErrorPostOnlyEndpoint")
        );
        assert_eq!(
            strings.task_error_text(&format!("{}\n", super::POST_ONLY_RESUME_MESSAGE)),
            translator.text("taskErrorPostOnlyResume")
        );
        assert_eq!(strings.task_error_text("disk full"), "disk full");
        Ok(())
    }

    /// 引擎 status 5（探测 / 插件解析）在任务表、详情、进度窗口与活动日志都是「准备中」，
    /// 不得退回 `TaskState::Pending` 的「等待中」；排队位置只属于未开始准备的等待任务。
    #[test]
    fn preparing_status_is_labelled_consistently_across_surfaces()
    -> Result<(), fluxdown_ui_i18n::I18nError> {
        let catalog = std::sync::Arc::new(fluxdown_ui_i18n::I18nCatalog::load_embedded()?);
        let translator = catalog.translator("en");
        let strings = DownloadStrings::from_translator(&translator);
        let task = |status: i32, queue_key: Option<i64>| {
            let dto = serde_json::from_value::<fluxdown_protocol::TaskDto>(serde_json::json!({
                "taskId": "t",
                "url": "https://www.youtube.com/watch?v=x",
                "fileName": "video.mp4",
                "saveDir": "/tmp",
                "status": status,
                "downloadedBytes": 0,
                "totalBytes": 0,
                "errorMessage": "",
                "createdAt": "1",
                "proxyUrl": "",
                "queueId": "main",
                "checksum": ""
            }))
            .expect("task");
            let mut view = crate::model::DownloadTaskView::local(&dto, None, false);
            view.queue_key = queue_key;
            view
        };
        let store = crate::model::TaskStore::default();
        store.replace_queue_keys(vec![30, 10, 20]);
        let preparing = translator.text("statusPreparing");
        let pending = translator.text("statusPending");

        let resolving = task(5, Some(30));
        assert_eq!(strings.task_state_label(&resolving).as_ref(), preparing);
        assert_eq!(
            strings.task_status_text(&resolving, &store).as_ref(),
            preparing
        );
        assert_eq!(strings.status_code_label(5).as_ref(), preparing);

        assert_eq!(
            strings.task_status_text(&task(0, None), &store).as_ref(),
            pending
        );
        assert_eq!(strings.status_code_label(0).as_ref(), pending);
        assert_eq!(
            strings
                .task_status_text(&task(0, Some(20)), &store)
                .as_ref(),
            translator.text("subtitleQueued").replace("{pos}", "2")
        );
        Ok(())
    }
}
