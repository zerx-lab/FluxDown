use super::{manifest::is_safe_relative_path, runtime::PluginError};

/// Public-safe plugin failure category. Original exception and parser text stays in logs.
#[derive(Debug, Clone)]
pub enum PluginFailureDiagnostic {
    ManifestUnreadable,
    ManifestUnknownField,
    ManifestJsonInvalid,
    ManifestIdentityInvalid,
    ManifestNameMissing,
    ManifestVersionInvalid,
    ManifestMinAppVersionInvalid,
    ManifestUnknownPermission,
    ManifestAuthPermissionMissing,
    ScriptCompileFailed,
    ScriptCompileFailedAt { line: u32, column: u32 },
    MissingFile { path: String },
    LoadFailed,
    NotDevPlugin,
}

impl PluginFailureDiagnostic {
    #[must_use]
    pub fn from_error(error: &PluginError) -> Option<Self> {
        Some(match error {
            PluginError::CompileFailed(message) => {
                // The exception can contain arbitrary plugin text. Export only numeric coordinates.
                message
                    .lines()
                    .find_map(|line| {
                        let (row, col) = line
                            .trim()
                            .strip_prefix("at eval_script:")?
                            .split_once(':')?;
                        Some(Self::ScriptCompileFailedAt {
                            line: row.parse().ok()?,
                            column: col.parse().ok()?,
                        })
                    })
                    .unwrap_or(Self::ScriptCompileFailed)
            }
            PluginError::LoadFailed(message) => missing_file(message).unwrap_or(Self::LoadFailed),
            PluginError::ManifestInvalid(message) => {
                // Manifest validation and archive/OS failures share this variant. Classify only
                // known host templates and never echo their interpolated values or parser text.
                match message.as_str() {
                    msg if msg.starts_with("读取 manifest") => Self::ManifestUnreadable,
                    msg if msg.starts_with("JSON 解析失败: unknown field ") => {
                        Self::ManifestUnknownField
                    }
                    msg if msg.starts_with("JSON 解析失败: ") => Self::ManifestJsonInvalid,
                    msg if msg.starts_with("identity '") => Self::ManifestIdentityInvalid,
                    "name 不可为空" => Self::ManifestNameMissing,
                    msg if msg.starts_with("version '") => Self::ManifestVersionInvalid,
                    msg if msg.starts_with("minAppVersion '") => Self::ManifestMinAppVersionInvalid,
                    msg if msg.starts_with("未知权限 '") => Self::ManifestUnknownPermission,
                    "声明 auth.entry 时 permissions 必须包含 auth" => {
                        Self::ManifestAuthPermissionMissing
                    }
                    _ => return missing_file(message),
                }
            }
            PluginError::NotDevPlugin(_) => Self::NotDevPlugin,
            _ => return None,
        })
    }
}

fn missing_file(message: &str) -> Option<PluginFailureDiagnostic> {
    let path = message.strip_prefix("包内缺少文件 '")?.strip_suffix('\'')?;
    (path.len() <= 256 && !path.contains(['\'', ':', '?', '#']) && is_safe_relative_path(path))
        .then(|| PluginFailureDiagnostic::MissingFile {
            path: path.to_owned(),
        })
}
