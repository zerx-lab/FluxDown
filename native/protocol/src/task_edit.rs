//! Unified task property editor. Secret material is write-only.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Non-secret task overrides, persisted and projected with the task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct TaskPropertiesDto {
    pub download_limit_bps: i64,
    pub user_agent: String,
    pub auth_ref: String,
    pub category: String,
    pub tags: Vec<String>,
    /// One-shot start time in Unix seconds; zero disables scheduling.
    pub scheduled_at: i64,
}

impl TaskPropertiesDto {
    /// Whether this task follows all existing defaults.
    pub fn is_empty(&self) -> bool {
        self.download_limit_bps == 0
            && self.user_agent.is_empty()
            && self.auth_ref.is_empty()
            && self.category.is_empty()
            && self.tags.is_empty()
            && self.scheduled_at == 0
    }
}

/// Fields which are meaningful for the selected protocol and task state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskEditCapabilities {
    pub request: bool,
    pub proxy: bool,
    pub storage: bool,
    pub rename: bool,
    pub segments: bool,
    pub schedule: bool,
}

/// Editable snapshot. Cookies are never returned; secret headers use `********`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskEditSnapshot {
    pub task_id: String,
    pub url: String,
    pub revision: String,
    pub save_dir: String,
    pub file_name: String,
    pub proxy_url: String,
    pub segments: i32,
    pub properties: TaskPropertiesDto,
    pub headers: HashMap<String, String>,
    pub cookies_configured: bool,
    pub capabilities: TaskEditCapabilities,
}

/// One save transaction. Omitted fields stay unchanged; empty values clear overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditTaskParams {
    pub task_id: String,
    pub expected_revision: String,
    pub save_dir: Option<String>,
    pub file_name: Option<String>,
    pub proxy_url: Option<String>,
    pub segments: Option<i32>,
    pub download_limit_bps: Option<i64>,
    pub user_agent: Option<String>,
    /// None preserves existing cookies; empty explicitly removes them.
    pub cookies: Option<String>,
    /// Secret header placeholders preserve their existing values.
    pub headers: Option<HashMap<String, String>>,
    pub auth_ref: Option<String>,
    /// Empty restores automatic classification.
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub scheduled_at: Option<i64>,
}
