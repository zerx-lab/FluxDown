//! 桌面「跳过此版本」偏好：用户在更新窗口跳过的版本（设备本地 agent 偏好，不进云同步）。
//!
//! agent 不再自动下载该版本，GPUI 顶栏更新按钮在它仅「可用」时不再出现；出现更高版本即恢复。
//! 桌面端从不自动弹出更新窗口：后台下载完成后只在主窗口顶栏显示「重启以更新」按钮。

use std::collections::BTreeMap;

use serde_json::Value;

/// 设备本地偏好：被「跳过此版本」的版本号。
pub const SKIPPED_VERSION_KEY: &str = "desktop.update_skipped_version";

/// 被跳过的版本（未设置或为空时为 `None`）。
#[must_use]
pub fn skipped_version(preferences: &BTreeMap<String, Value>) -> Option<&str> {
    preferences
        .get(SKIPPED_VERSION_KEY)
        .and_then(Value::as_str)
        .filter(|version| !version.is_empty())
}
