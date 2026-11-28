//! 数据目录 / 凭据文件的私有权限收紧。
//!
//! Unix 用 0700/0600；Windows 无安全的 std 封装，改用 `icacls` 子进程把 ACL 收紧为当前用户、
//! SYSTEM 与 Administrators（保留后两者，账户 SID 变化后管理员仍能接管，#1222）。与 agent
//! `state.rs` 同一组授权。
//!
//! Windows 便携版（`fluxdown_engine::data_dir::portable_data_dir`）不收紧：便携目录随文件夹在
//! 机器与账户之间迁移，旁边的程序文件同样只受文件夹权限保护；锁给单一 SID 后重装系统即连
//! 管理员都打不开。旧版本收紧过的便携目录在首次启动时一次性恢复为继承。

use std::path::Path;

/// 私有 ACL 在当前用户之外授予的账户：SYSTEM 与本机 Administrators 组。
#[cfg(windows)]
const EXTRA_SIDS: [&str; 2] = ["S-1-5-18", "S-1-5-32-544"];

/// 便携目录已恢复为继承父目录 ACL 的标记（与 agent 同名）。
#[cfg(windows)]
const INHERITED_ACL_MARKER: &str = ".acl-inherited";

/// 启动时收紧数据目录：其后在目录内创建的 DB、token、blob 目录自动继承。
pub async fn secure_data_dir(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await
    }
    #[cfg(windows)]
    {
        if is_portable_path(path) {
            return restore_inherited_acl(path).await;
        }
        restrict_to_current_user(path, true).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        restrict_to_current_user(path, true).await
    }
}

/// `path` 是否位于 Windows 便携数据目录内。
#[cfg(windows)]
fn is_portable_path(path: &Path) -> bool {
    fluxdown_engine::data_dir::portable_data_dir()
        .is_some_and(|portable| path.starts_with(portable))
}

/// `%SystemRoot%\System32\icacls.exe`；取不到时退回按 PATH 查找。
#[cfg(windows)]
fn icacls_program() -> std::path::PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| {
            std::path::PathBuf::from(root)
                .join("System32")
                .join("icacls.exe")
        })
        .filter(|path| path.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from("icacls"))
}

#[cfg(windows)]
async fn run_icacls(path: &Path, args: Vec<String>) -> Result<(), std::io::Error> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let path = path.to_owned();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new(icacls_program())
            .arg(path)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .await
    .map_err(std::io::Error::other)??;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("icacls failed ({status})")))
    }
}

/// 便携目录：一次性把整棵树恢复为继承所在文件夹的 ACL；失败只记录，结果写入标记不再重复遍历。
#[cfg(windows)]
async fn restore_inherited_acl(path: &Path) -> Result<(), std::io::Error> {
    let marker = path.join(INHERITED_ACL_MARKER);
    if tokio::fs::try_exists(&marker).await? {
        return Ok(());
    }
    let args = ["/reset", "/T", "/C", "/Q"].map(str::to_owned).to_vec();
    if let Err(error) = run_icacls(path, args).await {
        tracing::warn!(path = %path.display(), error = %error, "could not restore inherited ACLs on the portable data directory");
    }
    tokio::fs::write(&marker, b"").await
}

/// 仅当前用户（及 SYSTEM / Administrators）可访问 `path`；`inheritable` 为真时对目录内新建项
/// 可继承。便携目录内与非 Windows 为空操作。
#[cfg(windows)]
pub async fn restrict_to_current_user(
    path: &Path,
    inheritable: bool,
) -> Result<(), std::io::Error> {
    if is_portable_path(path) {
        return Ok(());
    }
    let sid = current_user_sid().await?;
    let inherit = if inheritable { "(OI)(CI)" } else { "" };
    let mut args = vec!["/inheritance:r".to_owned(), "/grant:r".to_owned()];
    args.extend(
        std::iter::once(sid.as_str())
            .chain(EXTRA_SIDS)
            .map(|sid| format!("*{sid}:{inherit}(F)")),
    );
    run_icacls(path, args).await
}

#[cfg(not(windows))]
pub async fn restrict_to_current_user(
    _path: &Path,
    _inheritable: bool,
) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(windows)]
async fn current_user_sid() -> Result<String, std::io::Error> {
    use std::os::windows::process::CommandExt;
    static SID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if let Some(sid) = SID.get() {
        return Ok(sid.clone());
    }
    let output = tokio::task::spawn_blocking(|| {
        std::process::Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    })
    .await
    .map_err(std::io::Error::other)??;
    if !output.status.success() {
        return Err(std::io::Error::other("whoami /user failed"));
    }
    let sid = parse_whoami_sid(&output.stdout)
        .ok_or_else(|| std::io::Error::other("could not parse current SID"))?;
    Ok(SID.get_or_init(|| sid).clone())
}

/// 从 `whoami /user /fo csv /nh` 的原始输出取 SID。
///
/// 输出按控制台 OEM 代码页编码（简体中文系统为 GBK），计算机名 / 用户名含非 ASCII 字符时不是
/// 合法 UTF-8。SID 是末尾字段且纯 ASCII，`,` 也不会出现在任何多字节编码的尾字节里，所以按字节
/// 切出末字段再解码，与代码页无关。
#[cfg(any(windows, test))]
fn parse_whoami_sid(output: &[u8]) -> Option<String> {
    let field = output.rsplit(|&byte| byte == b',').next()?;
    let sid = std::str::from_utf8(field).ok()?.trim().trim_matches('"');
    (sid.starts_with("S-1-")
        && sid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    .then(|| sid.to_owned())
}

#[cfg(test)]
mod tests {
    use super::parse_whoami_sid;

    #[test]
    fn parses_sid_from_whoami_csv() {
        assert_eq!(
            parse_whoami_sid(b"\"pc\\user\",\"S-1-5-21-1-2-3-1001\"\r\n").as_deref(),
            Some("S-1-5-21-1-2-3-1001")
        );
        // 简体中文系统的 OEM 代码页（GBK）输出，计算机名与用户名为「张三」：整行不是合法 UTF-8。
        assert_eq!(
            parse_whoami_sid(
                b"\"\xd5\xc5\xc8\xfd-PC\\\xd5\xc5\xc8\xfd\",\"S-1-5-21-1-2-3-1001\"\r\n"
            )
            .as_deref(),
            Some("S-1-5-21-1-2-3-1001")
        );
        assert_eq!(parse_whoami_sid(b"garbage"), None);
    }
}
