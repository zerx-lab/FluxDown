//! 服务器启动配置（环境变量）与首次运行初始化（token 生成）。
//!
//! | 环境变量 | 含义 | 默认 |
//! |---|---|---|
//! | `FLUXDOWN_DATA_DIR` | 数据目录（DB/日志） | 平台自动探测 |
//! | `FLUXDOWN_DATABASE_URL` | 数据库连接 URL（`sqlite:`/`postgres:`） | 数据目录下 SQLite |
//! | `FLUXDOWN_BIND` | HTTP 监听地址 | `0.0.0.0:17800` |
//! | `FLUXDOWN_WEBROOT` | SPA 静态资源目录 | 二进制同级 `./web` |
//! | `FLUXDOWN_DEMO` | 演示模式：仅允许下载内置本地演示文件 | 未设置（关闭） |
//! | `FLUXDOWN_DEMO_URL` | 演示模式：仅允许下载该 URL（覆盖内置） | 未设置（关闭） |

use std::path::PathBuf;

use fluxdown_engine::db::Db;
use fluxdown_engine::log_info;

/// 服务器进程级配置（全部来自环境变量）。
pub struct ServerConfig {
    pub bind: String,
    pub data_dir_override: Option<PathBuf>,
    pub database_url: Option<String>,
    pub webroot: PathBuf,
    /// 演示模式：`Some(url)` 时新任务仅允许下载该 URL（见 `host::demo_guard`）。
    pub demo_url: Option<String>,
}

impl ServerConfig {
    pub fn from_env() -> Self {
        let bind = std::env::var("FLUXDOWN_BIND").unwrap_or_else(|_| "0.0.0.0:17800".to_string());
        let data_dir_override = std::env::var_os("FLUXDOWN_DATA_DIR").map(PathBuf::from);
        let database_url = std::env::var("FLUXDOWN_DATABASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let webroot = std::env::var_os("FLUXDOWN_WEBROOT")
            .map(PathBuf::from)
            .unwrap_or_else(default_webroot);
        let demo_url = std::env::var("FLUXDOWN_DEMO_URL")
            .ok()
            .as_deref()
            .and_then(parse_demo_url)
            .or_else(|| demo_flag_enabled().then(|| builtin_demo_url(&bind)));
        Self {
            bind,
            data_dir_override,
            database_url,
            webroot,
            demo_url,
        }
    }
}

/// 归一化 `FLUXDOWN_DEMO_URL`：去掉首尾空白与误带的包裹引号
/// （Windows cmd 的 `set X="v" && …` 会把引号和尾部空格一并写进值），
/// 归一化后为空视为未开启。
fn parse_demo_url(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    for quote in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(quote) && s.ends_with(quote) {
            s = s[1..s.len() - 1].trim();
        }
    }
    (!s.is_empty()).then(|| s.to_string())
}

/// `FLUXDOWN_DEMO` 是否为真值（`1`/`true`/`yes`/`on`，忽略大小写）。
fn demo_flag_enabled() -> bool {
    std::env::var("FLUXDOWN_DEMO")
        .map(|v| flag_truthy(&v))
        .unwrap_or(false)
}

fn flag_truthy(v: &str) -> bool {
    matches!(
        v.trim()
            .trim_matches(['"', '\''])
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// 内置演示 URL：指向本进程自己挂载的 [`crate::demo::DEMO_FILE_PATH`]
/// （下载器与服务器同机，走 127.0.0.1 回环，不出外网）。
fn builtin_demo_url(bind: &str) -> String {
    let port = bind.rsplit(':').next().unwrap_or("17800");
    format!("http://127.0.0.1:{port}{}", crate::demo::DEMO_FILE_PATH)
}

/// 默认 SPA 目录：二进制同级 `./web`（取不到 exe 路径时退回 CWD 下 `web`）。
fn default_webroot() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("web")))
        .unwrap_or_else(|| PathBuf::from("web"))
}

/// 平台默认下载目录（复制自 `download_actor.rs` 的私有 helper）。
pub fn default_save_dir() -> String {
    if cfg!(target_os = "windows")
        && let Some(profile) = std::env::var_os("USERPROFILE")
    {
        let mut p = PathBuf::from(profile);
        p.push("Downloads");
        return p.to_string_lossy().into_owned();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push("Downloads");
        return p.to_string_lossy().into_owned();
    }
    ".".to_string()
}

/// 首次运行初始化：强制开启管理 API；token 为空则生成并持久化。
///
/// 返回生效的管理 token。新生成的 token 会打印到 stderr（headless 部署
/// 唯一一次可见的机会）。
pub async fn ensure_server_config(db: &Db) -> Result<String, fluxdown_engine::db::DbError> {
    // headless 服务器的存在意义就是远程管理——管理 API 恒开。
    db.set_config("local_server_api_enabled", "true").await?;

    // MCP 默认开（headless 场景面向自动化/AI 客户端），但尊重用户后续关闭：仅在缺省时播种。
    if db.get_config("local_server_mcp_enabled").await?.is_none() {
        db.set_config("local_server_mcp_enabled", "true").await?;
    }

    let token = db
        .get_config("local_server_token")
        .await?
        .unwrap_or_default();
    if !token.is_empty() {
        return Ok(token);
    }
    let token = format!("fxd_{}", uuid::Uuid::new_v4().simple());
    db.set_config("local_server_token", &token).await?;
    log_info!("[server] generated management token: {}", token);
    eprintln!("==============================================================");
    eprintln!("  FluxDown Server 首次运行，已生成管理 token：");
    eprintln!("    {token}");
    eprintln!("  用它登录 Web 界面 / 调用管理 API（Authorization: Bearer）。");
    eprintln!("==============================================================");
    Ok(token)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::parse_demo_url;

    #[test]
    fn parse_demo_url_strips_whitespace_and_wrapping_quotes() {
        let want = Some("https://example.com/demo.bin".to_string());
        assert_eq!(parse_demo_url("https://example.com/demo.bin"), want);
        // cmd.exe 的 `set X="v" && …`：引号 + 尾部空格一并进值。
        assert_eq!(parse_demo_url("\"https://example.com/demo.bin\" "), want);
        assert_eq!(parse_demo_url("'https://example.com/demo.bin'"), want);
    }

    #[test]
    fn parse_demo_url_empty_or_quotes_only_means_disabled() {
        assert_eq!(parse_demo_url(""), None);
        assert_eq!(parse_demo_url("   "), None);
        assert_eq!(parse_demo_url("\"\""), None);
    }

    #[test]
    fn parse_demo_url_keeps_interior_quotes_intact() {
        // 只剥一层「包裹」引号，不动 URL 内部字符。
        assert_eq!(
            parse_demo_url("\"https://e.com/a?q='x'\""),
            Some("https://e.com/a?q='x'".to_string())
        );
    }
}

#[cfg(test)]
mod builtin_tests {
    use super::{builtin_demo_url, flag_truthy};

    #[test]
    fn builtin_demo_url_uses_bind_port_over_loopback() {
        assert_eq!(
            builtin_demo_url("0.0.0.0:17800"),
            "http://127.0.0.1:17800/demo/file"
        );
        assert_eq!(
            builtin_demo_url("[::]:9000"),
            "http://127.0.0.1:9000/demo/file"
        );
    }

    #[test]
    fn flag_truthy_accepts_common_forms_and_quotes() {
        for v in ["1", "true", "YES", "On", "\"1\"", " true "] {
            assert!(flag_truthy(v), "{v:?} should be truthy");
        }
        for v in ["0", "false", "off", "", "  "] {
            assert!(!flag_truthy(v), "{v:?} should be falsy");
        }
    }
}
