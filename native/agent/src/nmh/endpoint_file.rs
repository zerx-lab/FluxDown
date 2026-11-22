//! Windows NMH 端点发现文件：中继经它找到本 agent 的命名管道。
//!
//! 管道名不再按账户名推导（`\\.\pipe\` 是全机共享命名空间：同名的域账户与本地账户会撞名，其他
//! 账户还能抢先建同名管道，让本 agent 建不起端点，或冒充 agent 收下扩展发来的下载请求与
//! Cookie）。agent 每次建端点时生成随机名 `\\.\pipe\fluxdown-nmh-<32 位小写十六进制>`，原子写入
//! `<engine 数据目录>/nmh/endpoint.json`（与浏览器清单同目录，安装版在用户私有的
//! `%LOCALAPPDATA%`）：`{"pipe":"\\\\.\\pipe\\fluxdown-nmh-…"}`。中继（`native/nmh/src/main.rs`）
//! 独立推导同一路径、每次连接前重读并校验名字形态；两侧测试用同一组字面量钉住。
//!
//! 文件只含管道名，不含凭据。agent 停止监听时删除（内容仍是自己的名字才删）；崩溃留下的旧文件
//! 指向已不存在的管道，中继按「App 未运行」处理并拉起 agent，新 agent 会覆盖它。

#[cfg(windows)]
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 发现文件名，位于 `<engine 数据目录>/nmh/`。
pub(super) const FILE_NAME: &str = "endpoint.json";
/// 管道名前缀；完整名 = 前缀 + 32 位小写十六进制。
pub(super) const PIPE_PREFIX: &str = r"\\.\pipe\fluxdown-nmh-";
/// 随机后缀长度（UUID simple 形态）。
const SUFFIX_LEN: usize = 32;

#[derive(Serialize, Deserialize)]
struct Record {
    pipe: String,
}

/// `data_dir`（engine 数据目录）下的发现文件路径。
pub(super) fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join("nmh").join(FILE_NAME)
}

/// 本 agent 的发现文件路径（engine 数据目录与浏览器清单目录同源，见 `runtime::engine_data_dir`）。
#[cfg(windows)]
pub(super) fn path() -> PathBuf {
    path_in(&crate::runtime::engine_data_dir())
}

/// 新的随机管道名。
#[cfg(any(windows, test))]
pub(super) fn new_pipe_name() -> String {
    format!("{PIPE_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

/// 名字是否为本契约生成的 NMH 管道名（前缀 + 32 位小写十六进制，不接受其他路径或远端管道）。
pub(super) fn is_pipe_name(name: &str) -> bool {
    name.strip_prefix(PIPE_PREFIX).is_some_and(|suffix| {
        suffix.len() == SUFFIX_LEN
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// 解析发现文件内容；结构不对或名字形态不对时返回 `None`。
pub(super) fn parse(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<Record>(bytes)
        .ok()
        .map(|record| record.pipe)
        .filter(|pipe| is_pipe_name(pipe))
}

/// 序列化发现文件内容。
fn encode(pipe: &str) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&Record {
        pipe: pipe.to_owned(),
    })
}

/// 读取当前发布的管道名：文件不存在为 `NotFound`（agent 未运行），内容无效为 `InvalidData`。
#[cfg(windows)]
pub(super) fn read_published() -> io::Result<String> {
    let path = path();
    let bytes = std::fs::read(&path)?;
    parse(&bytes).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("NMH endpoint file {} is invalid", path.display()),
        )
    })
}

/// 原子发布 `pipe`（临时文件 + rename），读取方不会看到半截 JSON。
#[cfg(windows)]
pub(super) async fn publish(pipe: &str) -> io::Result<()> {
    let path = path();
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(directory).await?;
    let bytes = encode(pipe).map_err(io::Error::other)?;
    let temp = directory.join(format!(".endpoint.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = async {
        tokio::fs::write(&temp, &bytes).await?;
        tokio::fs::rename(&temp, &path).await
    }
    .await;
    if result.is_err()
        && let Err(error) = tokio::fs::remove_file(&temp).await
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %temp.display(), error = %error, "could not remove the failed NMH endpoint temp file");
    }
    result
}

/// 停止监听时撤下发布：文件仍指向 `pipe` 才删除，不误删接手的新实例刚写的文件。
#[cfg(windows)]
pub(super) async fn retract(pipe: &str) {
    let path = path();
    match tokio::fs::read(&path).await {
        Ok(bytes) if parse(&bytes).as_deref() == Some(pipe) => {}
        Ok(_) => return,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "could not read the NMH endpoint file to retract it");
            return;
        }
    }
    if let Err(error) = tokio::fs::remove_file(&path).await
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %error, "could not remove the NMH endpoint file");
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{FILE_NAME, encode, is_pipe_name, new_pipe_name, parse, path_in};

    /// 与 `native/nmh/src/main.rs` 的 `windows_endpoint_contract` 使用同一组字面量。
    #[test]
    fn windows_endpoint_contract() {
        assert_eq!(FILE_NAME, "endpoint.json");
        assert_eq!(
            path_in(Path::new("root")),
            Path::new("root").join("nmh").join("endpoint.json")
        );
        let valid = r"\\.\pipe\fluxdown-nmh-0123456789abcdef0123456789abcdef";
        assert!(is_pipe_name(valid));
        assert_eq!(
            parse(br#"{"pipe":"\\\\.\\pipe\\fluxdown-nmh-0123456789abcdef0123456789abcdef"}"#)
                .as_deref(),
            Some(valid)
        );
        for invalid in [
            r"\\.\pipe\fluxdown-nmh-0123456789ABCDEF0123456789abcdef",
            r"\\.\pipe\fluxdown-nmh-0123456789abcdef",
            r"\\server\pipe\fluxdown-nmh-0123456789abcdef0123456789abcdef",
            r"\\.\pipe\fluxdown-alice",
            r"C:\Users\alice\file",
        ] {
            assert!(!is_pipe_name(invalid), "{invalid}");
        }
        assert_eq!(parse(b"{broken"), None);
        assert_eq!(parse(br#"{"pipe":"\\\\.\\pipe\\fluxdown-alice"}"#), None);
    }

    #[test]
    fn generated_names_round_trip_and_differ() {
        let first = new_pipe_name();
        let second = new_pipe_name();
        assert!(is_pipe_name(&first), "{first}");
        assert_ne!(first, second);
        let bytes = encode(&first).expect("encode endpoint");
        assert_eq!(parse(&bytes), Some(first));
    }
}
