//! 本地设备互联（device link）错误类型。

/// 设备互联子系统的统一错误。HTTP 宿主层把它映射为响应状态码，
/// Dart 宿主层把它转成 `LinkEvent` 的错误信号。
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// 配对码不存在 / 已过期 / 已被使用。
    #[error("pairing code invalid or expired")]
    InvalidCode,

    /// 配对会话不存在或已过期（confirm 阶段找不到 hello 建立的会话）。
    #[error("pairing session not found or expired")]
    SessionExpired,

    /// 对端提交的身份签名校验失败（Ed25519 verify 失败）——身份与临时密钥绑定被破坏。
    #[error("peer identity signature verification failed")]
    BadSignature,

    /// 载荷字段非法（长度不符 / base64 解码失败 / 缺字段等）。
    #[error("invalid link payload: {0}")]
    BadPayload(String),

    /// 数据面链路鉴权失败（HMAC 不匹配 / 时间戳过期 / 设备未配对）。
    #[error("link authentication failed")]
    Unauthorized,

    /// 目标设备当前不可达（所有传输策略都失败）。
    #[error("peer unreachable")]
    Unreachable,

    /// 底层持久化错误。
    #[error("link store error: {0}")]
    Store(#[from] crate::db::DbError),

    /// 网络 / IO 错误（探测、HTTP 请求、mDNS）。
    #[error("link io error: {0}")]
    Io(String),

    /// 该宿主未启用设备互联能力。
    #[error("device link not available on this host")]
    Unavailable,
}

/// 便捷别名。
pub type LinkResult<T> = Result<T, LinkError>;
