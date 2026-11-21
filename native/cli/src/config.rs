//! 持久化 CLI 配置 —— `fluxdown config set/unset/get/list` 写入的本地配置文件，
//! 效果类似 `go env -w`：一次设置长期生效，无需每次导出环境变量。
//!
//! ## 优先级
//!
//! 每个配置项的生效值按「显式 > 环境 > 持久化配置 > 内置默认」解析：
//!
//! 1. 命令行 flag（`--token X`）
//! 2. 环境变量（`FLUXDOWN_TOKEN` —— 由 clap 的 `env=` 合并进 flag）
//! 3. 本文件的持久化配置（`config set` 写入）
//! 4. 内置默认（仅 `url`，默认 `http://127.0.0.1:17800`）
//!
//! 前两层由 `main.rs` 的 clap 层处理；本模块负责第 3 层。
//!
//! ## 存储位置
//!
//! 跨平台配置目录下的 `cli.toml`（[`directories::ProjectDirs`] 解析）：
//!
//! | 平台 | 路径 |
//! |---|---|
//! | Windows | `%APPDATA%\zerx\fluxdown\config\cli.toml` |
//! | Linux | `$XDG_CONFIG_HOME/fluxdown/cli.toml`（默认 `~/.config/fluxdown/`） |
//! | macOS | `~/Library/Application Support/dev.zerx.fluxdown/cli.toml` |
//!
//! ## 安全
//!
//! `token` 以明文存储（与 `gh`/`aws`/`docker` CLI 一致）。在 Unix 上文件权限
//! 被设为 `0600`（仅属主可读写）；Windows 依赖用户目录的 ACL 继承。

use std::fs;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::exit::ExitCode;

/// 配置文件名（放在平台配置目录下）。
const CONFIG_FILE: &str = "cli.toml";

/// 可持久化的配置键（`config set <key> <value>` 的合法 key）。
pub const KEYS: [&str; 3] = ["url", "token", "timeout"];

/// 配置操作错误，携带最贴近的退出码。手写 `Display`/`Error`（与 `client.rs`
/// 的 `ClientError` 风格一致，CLI crate 不引入 `thiserror`）。
#[derive(Debug)]
pub enum ConfigError {
    /// 无法定位平台配置目录。
    NoConfigDir,
    /// 未知的配置键。
    UnknownKey(String),
    /// 键的值格式非法（如 timeout 非数字）。
    InvalidValue {
        /// 出错的键名。
        key: String,
        /// 具体原因。
        reason: String,
    },
    /// 读写配置文件失败。
    Io {
        /// 涉及的文件路径。
        path: String,
        /// 底层 I/O 错误。
        source: std::io::Error,
    },
    /// TOML 解析失败（文件被手工改坏）。
    Parse {
        /// 涉及的文件路径。
        path: String,
        /// 底层解析错误。
        source: toml::de::Error,
    },
    /// TOML 序列化失败。
    Serialize(toml::ser::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NoConfigDir => {
                f.write_str("cannot determine a config directory on this platform")
            }
            ConfigError::UnknownKey(k) => {
                write!(
                    f,
                    "unknown config key: {k} (valid keys: url, token, timeout, log_level)"
                )
            }
            ConfigError::InvalidValue { key, reason } => {
                write!(f, "invalid value for {key}: {reason}")
            }
            ConfigError::Io { path, source } => {
                write!(f, "config file I/O error at {path}: {source}")
            }
            ConfigError::Parse { path, source } => {
                write!(f, "failed to parse config file {path}: {source}")
            }
            ConfigError::Serialize(e) => write!(f, "failed to serialize config: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            ConfigError::Parse { source, .. } => Some(source),
            ConfigError::Serialize(e) => Some(e),
            _ => None,
        }
    }
}

impl ConfigError {
    /// 映射为 CLI 退出码：键/值错误归 `BadRequest`，其余归 `Unknown`。
    #[must_use]
    pub fn exit(&self) -> ExitCode {
        match self {
            ConfigError::UnknownKey(_) | ConfigError::InvalidValue { .. } => ExitCode::BadRequest,
            _ => ExitCode::Unknown,
        }
    }
}

/// 持久化的 CLI 配置。所有字段可选：未设置的项回退到环境变量/默认值。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliConfig {
    /// 服务基址（对应 `--url` / `FLUXDOWN_URL`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// 管理 API token（对应 `--token` / `FLUXDOWN_TOKEN`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// 单请求超时秒数（对应 `--timeout`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// 解析配置文件路径（不保证存在）。
///
/// # Examples
///
/// ```no_run
/// use fluxdown_cli::config::config_path;
///
/// let p = config_path().expect("a config dir should exist");
/// assert!(p.ends_with("cli.toml"));
/// ```
pub fn config_path() -> Result<PathBuf, ConfigError> {
    let dirs = ProjectDirs::from("dev", "zerx", "fluxdown").ok_or(ConfigError::NoConfigDir)?;
    Ok(dirs.config_dir().join(CONFIG_FILE))
}

impl CliConfig {
    /// 从磁盘加载配置。文件不存在时返回空配置（不是错误）。
    pub fn load() -> Result<Self, ConfigError> {
        let path = config_path()?;
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// 将配置写入磁盘（创建父目录；Unix 上设 `0600` 权限）。
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let text = toml::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        fs::write(&path, &text).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        restrict_permissions(&path)?;
        Ok(())
    }

    /// 按 key 读取单个值（渲染为字符串）。未知 key → 错误；未设置 → `None`。
    pub fn get(&self, key: &str) -> Result<Option<String>, ConfigError> {
        match key {
            "url" => Ok(self.url.clone()),
            "token" => Ok(self.token.clone()),
            "timeout" => Ok(self.timeout.map(|t| t.to_string())),
            other => Err(ConfigError::UnknownKey(other.to_string())),
        }
    }

    /// 按 key 设置单个值。`timeout` 会校验为正整数。
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), ConfigError> {
        match key {
            "url" => self.url = Some(value.to_string()),
            "token" => self.token = Some(value.to_string()),
            "timeout" => {
                let secs: u64 = value.parse().map_err(|_| ConfigError::InvalidValue {
                    key: key.to_string(),
                    reason: format!("expected a positive integer, got {value:?}"),
                })?;
                if secs == 0 {
                    return Err(ConfigError::InvalidValue {
                        key: key.to_string(),
                        reason: "timeout must be at least 1 second".to_string(),
                    });
                }
                self.timeout = Some(secs);
            }
            other => return Err(ConfigError::UnknownKey(other.to_string())),
        }
        Ok(())
    }

    /// 按 key 清除单个值。未知 key → 错误。
    pub fn unset(&mut self, key: &str) -> Result<(), ConfigError> {
        match key {
            "url" => self.url = None,
            "token" => self.token = None,
            "timeout" => self.timeout = None,
            other => return Err(ConfigError::UnknownKey(other.to_string())),
        }
        Ok(())
    }

    /// 列出全部键的当前值（未设置的键值为 `None`），按 [`KEYS`] 顺序。
    #[must_use]
    pub fn entries(&self) -> Vec<(&'static str, Option<String>)> {
        KEYS.iter()
            .map(|&k| {
                // key 恒合法（来自 KEYS），get 不会返回 UnknownKey。
                let v = self.get(k).ok().flatten();
                (k, v)
            })
            .collect()
    }
}

/// Unix：将配置文件权限收紧为 `0600`（仅属主可读写），保护明文 token。
#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, perms).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })
}

/// 非 Unix（Windows）：无 POSIX 权限位，依赖用户配置目录的 ACL 继承。
#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::CliConfig;

    #[test]
    fn set_get_roundtrip() {
        let mut c = CliConfig::default();
        c.set("url", "http://example:1234").unwrap();
        c.set("token", "fxd_abc").unwrap();
        c.set("timeout", "45").unwrap();
        assert_eq!(
            c.get("url").unwrap().as_deref(),
            Some("http://example:1234")
        );
        assert_eq!(c.get("token").unwrap().as_deref(), Some("fxd_abc"));
        assert_eq!(c.get("timeout").unwrap().as_deref(), Some("45"));
    }

    #[test]
    fn unset_clears_value() {
        let mut c = CliConfig::default();
        c.set("token", "x").unwrap();
        c.unset("token").unwrap();
        assert_eq!(c.get("token").unwrap(), None);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let mut c = CliConfig::default();
        assert!(c.set("nope", "v").is_err());
        assert!(c.get("nope").is_err());
        assert!(c.unset("nope").is_err());
    }

    #[test]
    fn timeout_must_be_positive_integer() {
        let mut c = CliConfig::default();
        assert!(c.set("timeout", "abc").is_err());
        assert!(c.set("timeout", "0").is_err());
        assert!(c.set("timeout", "1").is_ok());
    }

    #[test]
    fn toml_roundtrip_omits_unset_fields() {
        let mut c = CliConfig::default();
        c.set("token", "secret").unwrap();
        let text = toml::to_string_pretty(&c).unwrap();
        // 只有设置过的字段才落盘。
        assert!(text.contains("token"));
        assert!(!text.contains("url"));
        assert!(!text.contains("timeout"));
        let back: CliConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn entries_lists_all_keys_in_order() {
        let mut c = CliConfig::default();
        c.set("url", "u").unwrap();
        let entries = c.entries();
        let keys: Vec<_> = entries.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec!["url", "token", "timeout"]);
        assert_eq!(entries[0].1.as_deref(), Some("u"));
        assert_eq!(entries[1].1, None);
    }
}
