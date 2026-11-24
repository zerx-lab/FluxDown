//! FFI 错误面：`FluxError`（命令失败）与 `HostErrorDto`（`HostSignalDto::Fatal` 载荷）。
//!
//! 映射自 `fluxdown_protocol::RpcErrorObject`：先按 `reason`（`ErrorReason` wire 名）本地化，
//! 再按 `code` 回退；`retryable` 决定是否展示重试入口。

use fluxdown_protocol::{
    ApplicationErrorCode, ErrorReason, INTERNAL_ERROR_CODE, INVALID_PARAMS_CODE,
    INVALID_REQUEST_CODE, METHOD_NOT_FOUND_CODE, PARSE_ERROR_CODE, PluginFailureDetail,
    RpcErrorData, RpcErrorObject,
};

/// `ApplicationErrorCode`。
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum ErrorCodeDto {
    ProtocolIncompatible,
    Unauthorized,
    InvalidArgument,
    NotFound,
    Conflict,
    Unavailable,
    Timeout,
    Cancelled,
    Unsupported,
    Internal,
}

impl From<ApplicationErrorCode> for ErrorCodeDto {
    fn from(code: ApplicationErrorCode) -> Self {
        match code {
            ApplicationErrorCode::ProtocolIncompatible => Self::ProtocolIncompatible,
            ApplicationErrorCode::Unauthorized => Self::Unauthorized,
            ApplicationErrorCode::InvalidArgument => Self::InvalidArgument,
            ApplicationErrorCode::NotFound => Self::NotFound,
            ApplicationErrorCode::Conflict => Self::Conflict,
            ApplicationErrorCode::Unavailable => Self::Unavailable,
            ApplicationErrorCode::Timeout => Self::Timeout,
            ApplicationErrorCode::Cancelled => Self::Cancelled,
            ApplicationErrorCode::Unsupported => Self::Unsupported,
            ApplicationErrorCode::Internal => Self::Internal,
        }
    }
}

/// Redacted plugin operation diagnostic copied from the daemon's typed error.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct PluginDiagnosticDto {
    pub kind: String,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl PluginDiagnosticDto {
    fn from_detail(detail: PluginFailureDetail) -> Option<Self> {
        use PluginFailureDetail as Detail;

        let (kind, path, position) = match detail {
            Detail::ManifestUnreadable => ("manifestUnreadable", None, None),
            Detail::ManifestUnknownField => ("manifestUnknownField", None, None),
            Detail::ManifestJsonInvalid => ("manifestJsonInvalid", None, None),
            Detail::ManifestIdentityInvalid => ("manifestIdentityInvalid", None, None),
            Detail::ManifestNameMissing => ("manifestNameMissing", None, None),
            Detail::ManifestVersionInvalid => ("manifestVersionInvalid", None, None),
            Detail::ManifestMinAppVersionInvalid => ("manifestMinAppVersionInvalid", None, None),
            Detail::ManifestUnknownPermission => ("manifestUnknownPermission", None, None),
            Detail::ManifestAuthPermissionMissing => ("manifestAuthPermissionMissing", None, None),
            Detail::ScriptCompileFailed => ("scriptCompileFailed", None, None),
            Detail::ScriptCompileFailedAt { line, column } => {
                ("scriptCompileFailedAt", None, Some((line, column)))
            }
            Detail::MissingFile { path } => ("missingFile", Some(path), None),
            Detail::LoadFailed => ("loadFailed", None, None),
            Detail::NotDevPlugin => ("notDevPlugin", None, None),
            Detail::Unknown => return None,
        };
        Some(Self {
            kind: kind.to_owned(),
            path,
            line: position.map(|(line, _)| line),
            column: position.map(|(_, column)| column),
        })
    }
}

/// 不可恢复的会话错误（协议不兼容、鉴权失败、主机退出等）。
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct HostErrorDto {
    pub code: ErrorCodeDto,
    /// `ErrorReason` 的 wire 名（camelCase）。
    pub reason: Option<String>,
    pub retryable: bool,
    pub message: String,
    pub plugin_diagnostic: Option<PluginDiagnosticDto>,
}

impl HostErrorDto {
    pub(crate) fn new(code: ErrorCodeDto, message: impl Into<String>) -> Self {
        Self {
            code,
            reason: None,
            retryable: false,
            message: message.into(),
            plugin_diagnostic: None,
        }
    }
}

/// 命令 / 打开会话失败。
#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum FluxError {
    /// 主机返回了应用错误（`RpcErrorData`）。
    #[error("rpc error {code:?}: {detail}")]
    Rpc {
        code: ErrorCodeDto,
        /// `ErrorReason` 的 wire 名（camelCase）。
        reason: Option<String>,
        retryable: bool,
        /// 人类可读说明（不叫 `message`：与 Kotlin `Throwable.message` 冲突）。
        detail: String,
        plugin_diagnostic: Option<PluginDiagnosticDto>,
    },
    /// 连接 / IO 失败。
    #[error("transport error: {detail}")]
    Transport { detail: String },
    /// 会话已关闭。
    #[error("session closed")]
    Closed,
}

impl FluxError {
    pub(crate) fn transport(detail: impl Into<String>) -> Self {
        Self::Transport {
            detail: detail.into(),
        }
    }

    pub(crate) fn rpc(code: ErrorCodeDto, detail: impl Into<String>) -> Self {
        Self::Rpc {
            code,
            reason: None,
            retryable: false,
            detail: detail.into(),
            plugin_diagnostic: None,
        }
    }

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::rpc(ErrorCodeDto::InvalidArgument, message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::rpc(ErrorCodeDto::Internal, message)
    }
}

/// `ErrorReason` 的 wire 名。
fn reason_wire(reason: ErrorReason) -> Option<String> {
    match serde_json::to_value(reason) {
        Ok(serde_json::Value::String(name)) => Some(name),
        _ => None,
    }
}

impl From<RpcErrorObject> for FluxError {
    fn from(error: RpcErrorObject) -> Self {
        if let Some(data) = error.data {
            return Self::Rpc {
                code: data.code.into(),
                reason: data.reason.and_then(reason_wire),
                retryable: data.retryable,
                detail: error.message,
                plugin_diagnostic: data
                    .plugin_detail
                    .and_then(PluginDiagnosticDto::from_detail),
            };
        }
        // 没有应用错误详情的 JSON-RPC 协议级错误：按标准 code 归类。
        let code = match error.code {
            METHOD_NOT_FOUND_CODE => ErrorCodeDto::Unsupported,
            PARSE_ERROR_CODE | INVALID_REQUEST_CODE | INVALID_PARAMS_CODE => {
                ErrorCodeDto::InvalidArgument
            }
            INTERNAL_ERROR_CODE => ErrorCodeDto::Internal,
            _ => ErrorCodeDto::Internal,
        };
        Self::rpc(code, error.message)
    }
}

impl From<FluxError> for HostErrorDto {
    fn from(error: FluxError) -> Self {
        match error {
            FluxError::Rpc {
                code,
                reason,
                retryable,
                detail,
                plugin_diagnostic,
            } => Self {
                code,
                reason,
                retryable,
                message: detail,
                plugin_diagnostic,
            },
            FluxError::Transport { detail } => Self {
                code: ErrorCodeDto::Unavailable,
                reason: None,
                retryable: true,
                message: detail,
                plugin_diagnostic: None,
            },
            FluxError::Closed => Self::new(ErrorCodeDto::Cancelled, "session closed"),
        }
    }
}

impl From<HostErrorDto> for FluxError {
    fn from(error: HostErrorDto) -> Self {
        Self::Rpc {
            code: error.code,
            reason: error.reason,
            retryable: error.retryable,
            detail: error.message,
            plugin_diagnostic: error.plugin_diagnostic,
        }
    }
}

/// 批量建任务的单条失败（`RpcErrorData` 不带文案，`message` 只放 `field`：指向违规的条目字段）。
impl From<RpcErrorData> for HostErrorDto {
    fn from(data: RpcErrorData) -> Self {
        Self {
            code: data.code.into(),
            reason: data.reason.and_then(reason_wire),
            retryable: data.retryable,
            message: data.field.unwrap_or_default(),
            plugin_diagnostic: data
                .plugin_detail
                .and_then(PluginDiagnosticDto::from_detail),
        }
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{ApplicationErrorCode, ErrorReason, RpcErrorData, RpcErrorObject};

    use super::{ErrorCodeDto, FluxError};

    #[test]
    fn application_error_keeps_code_reason_and_retryable() {
        let data = RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
            .with_reason(ErrorReason::TargetDeviceOffline);
        let error: FluxError = RpcErrorObject::application("offline", data).into();
        assert!(matches!(
            &error,
            FluxError::Rpc {
                code: ErrorCodeDto::Unavailable,
                reason: Some(reason),
                retryable: true,
                ..
            } if reason == "targetDeviceOffline"
        ));
    }

    #[test]
    fn plugin_diagnostic_survives_rpc_and_session_error_conversion() {
        use fluxdown_protocol::PluginFailureDetail;

        use super::{HostErrorDto, PluginDiagnosticDto};

        let mut data = RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
            .with_reason(ErrorReason::PluginPackageInvalid);
        data.plugin_detail = Some(PluginFailureDetail::ScriptCompileFailedAt {
            line: 8,
            column: 13,
        });
        let error = FluxError::from(RpcErrorObject::application("invalid plugin package", data));
        let dto = HostErrorDto::from(error);
        assert_eq!(dto.message, "invalid plugin package");
        assert!(matches!(
            FluxError::from(dto),
            FluxError::Rpc {
                plugin_diagnostic: Some(PluginDiagnosticDto {
                    kind, path: None, line: Some(8), column: Some(13)
                }),
                ..
            } if kind == "scriptCompileFailedAt"
        ));
    }

    #[test]
    fn protocol_level_error_maps_by_json_rpc_code() {
        let error: FluxError = RpcErrorObject {
            code: fluxdown_protocol::METHOD_NOT_FOUND_CODE,
            message: "no such method".to_owned(),
            data: None,
        }
        .into();
        assert!(matches!(
            error,
            FluxError::Rpc {
                code: ErrorCodeDto::Unsupported,
                reason: None,
                retryable: false,
                ..
            }
        ));
    }
}
