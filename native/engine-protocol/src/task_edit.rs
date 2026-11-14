//! Stateless unified-task-editor conversions.
use fluxdown_engine::task_edit::{EditSnapshot, EditTask, TaskProperties};
use fluxdown_protocol::{
    EditTaskParams, TaskEditCapabilities, TaskEditSnapshot, TaskPropertiesDto,
};

/// Project non-secret task properties.
pub fn task_properties_to_dto(value: TaskProperties) -> TaskPropertiesDto {
    TaskPropertiesDto {
        download_limit_bps: value.download_limit_bps,
        user_agent: value.user_agent,
        auth_ref: value.auth_ref,
        category: value.category,
        tags: value.tags,
        scheduled_at: value.scheduled_at,
    }
}

/// Project a redacted editable snapshot.
pub fn task_edit_snapshot_to_dto(value: EditSnapshot) -> TaskEditSnapshot {
    TaskEditSnapshot {
        task_id: value.task_id,
        url: value.url,
        revision: value.revision,
        save_dir: value.save_dir,
        file_name: value.file_name,
        proxy_url: value.proxy_url,
        segments: value.segments,
        properties: task_properties_to_dto(value.properties),
        headers: value.headers,
        cookies_configured: value.cookies_configured,
        capabilities: TaskEditCapabilities {
            request: value.capabilities.request,
            proxy: value.capabilities.proxy,
            storage: value.capabilities.storage,
            rename: value.capabilities.rename,
            segments: value.capabilities.segments,
            schedule: value.capabilities.schedule,
        },
    }
}

/// Translate one partial save without exposing credentials in projections.
pub fn task_edit_to_engine(value: EditTaskParams) -> EditTask {
    EditTask {
        task_id: value.task_id,
        expected_revision: value.expected_revision,
        save_dir: value.save_dir,
        file_name: value.file_name,
        proxy_url: value.proxy_url,
        segments: value.segments,
        download_limit_bps: value.download_limit_bps,
        user_agent: value.user_agent,
        cookies: value.cookies,
        headers: value.headers,
        auth_ref: value.auth_ref,
        category: value.category,
        tags: value.tags,
        scheduled_at: value.scheduled_at,
    }
}
