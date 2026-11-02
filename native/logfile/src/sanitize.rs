//! 日志脱敏规则表（单一来源）。
//!
//! 本 crate 零第三方依赖，只导出数据；各宿主自行用自己的正则引擎编译后套用。
//! 规则按顺序依次应用：每条是 `(regex 模式, 替换串)`，模式使用 Rust `regex` crate 语法
//! （内联 `(?i)` 大小写不敏感标志），替换串使用 `$1` 风格的捕获组引用。

/// 日志/导出文本脱敏规则，依次对全文套用。
///
/// 覆盖：URL userinfo、Telegram bot 令牌路径、令牌/密钥类 query 参数、超长 query、
/// Cookie / Authorization 头、代理账号密码、用户目录（Linux `/home/<name>/`、
/// Windows `X:\Users\<name>\`）。
pub const SANITIZE_PATTERNS: &[(&str, &str)] = &[
    (r"(?i)([\w+.-]+://)[^:/\s@]+:[^@\s]+@", "$1***@"),
    // Telegram bot 令牌在路径里：/bot<id>:<token>/
    (r"(?i)(/bot)\d+:[\w-]+", "$1[REDACTED]"),
    // 令牌/密钥类 query 参数，无论值长短。
    (
        r"(?i)([?&](?:[\w.-]*(?:token|key|secret|password|passwd|pwd|sig|signature|auth|credential)[\w.-]*)=)[^&\s,)\]}>]+",
        "$1[REDACTED]",
    ),
    (
        r#"(?i)(https?://[^?\s]{3,})\?[^\s,)\]}>"]{50,}"#,
        "$1?[QUERY_REDACTED]",
    ),
    (r"(?i)(cookie\b[^:\r\n]*:\s*)\S+", "$1[REDACTED]"),
    (
        r"(?i)(authorization\b[^:\r\n]*:\s*)(?:\S+\s+)?\S+",
        "$1[REDACTED]",
    ),
    (
        r"(?i)(proxy[_\s]?(?:password|username)\s*[=:]\s*)\S+",
        "$1[REDACTED]",
    ),
    (r"/home/[^/\s]+/", "/home/***/"),
    (r"(?i)([A-Z]:\\users\\)[^\\\s]+\\", "$1***\\"),
];
