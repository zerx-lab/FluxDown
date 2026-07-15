//! 可选外部组件管理（v1：ffmpeg + yt-dlp）。
//!
//! 组件是宿主侧受控的外部可执行程序，**不随安装包分发**，运行时按需由用户在
//! 设置「组件」页主动触发下载（合规边界）。它们与插件沙箱（QuickJS）是两套
//! 正交的信任模型，本模块不与插件系统的脚本执行交互——但已安装的组件会被
//! 授权插件经 `flux.ffmpeg` / `flux.ytdlp` 门面调用（见 [`crate::plugin`]）。
//!
//! - [`ffmpeg`]：音视频处理，服务 DASH/轨对任务的 mux（BtbN 归档，解压取单文件）。
//! - [`ytdlp`]：站点媒体提取器，服务插件 resolve 平面的直链提取（单文件二进制）。
//!
//! 两者共用同一套路径来源模型（[`ComponentSource`]，manual→managed→system）、
//! 错误类型（[`ComponentError`]）与安装底座（[`download_to_file`]/[`fetch_github_json`]）。

mod ffmpeg;
mod ytdlp;

pub use ffmpeg::*;
pub use ytdlp::*;

use std::path::PathBuf;

/// 组件生效路径的来源。ffmpeg / yt-dlp 共用；`as_str` 为稳定 wire 字符串
/// （跨 hub 信号 / server JSON / Dart 徽章共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentSource {
    /// 用户手动指定路径（config）。
    Manual,
    /// 数据目录 `bin/` 下的托管安装。
    Managed,
    /// 系统 PATH 中找到。
    System,
    /// 未找到任何可用二进制。
    None,
}

impl ComponentSource {
    /// 稳定的 wire 字符串（跨 hub 信号 / server JSON 共用）。
    pub fn as_str(self) -> &'static str {
        match self {
            ComponentSource::Manual => "manual",
            ComponentSource::Managed => "managed",
            ComponentSource::System => "system",
            ComponentSource::None => "none",
        }
    }
}

/// 组件操作错误（ffmpeg / yt-dlp 共用）。
#[derive(thiserror::Error, Debug)]
pub enum ComponentError {
    /// 当前平台无托管构建——请用系统安装或手动指定路径。
    #[error("managed install not supported on this platform")]
    Unsupported,
    #[error("http error: {0}")]
    Http(String),
    #[error("asset not found: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("db error: {0}")]
    Db(String),
    #[error("archive error: {0}")]
    Archive(String),
    /// 安装后可执行探测失败（下载损坏/架构不符）。
    #[error("installed binary failed verification: {0}")]
    Verify(String),
}

/// 扫描系统 PATH 寻找指定可执行文件（含 Windows 的 `.exe` 后缀由调用方带入）。
pub fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// GitHub Release API JSON 拉取（带 `User-Agent`/`Accept` 头）。ffmpeg / yt-dlp
/// 安装流程共用。
#[cfg(feature = "components")]
pub(crate) async fn fetch_github_json(
    client: &reqwest::Client,
    url: &str,
) -> Result<serde_json::Value, ComponentError> {
    let resp = client
        .get(url)
        .header("User-Agent", "FluxDown")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| ComponentError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(ComponentError::Http(format!(
            "GitHub API returned {}",
            resp.status()
        )));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| ComponentError::Http(e.to_string()))
}

/// 官网组件版本镜像的基地址。版本列表拉取优先经此转发（服务端持 token +
/// 24h 缓存，规避 GitHub 匿名 API 每 IP 60/h 限流与直连 api.github.com 的
/// 网络问题），失败回退直连 GitHub。
#[cfg(feature = "components")]
const MIRROR_BASE: &str = "https://fluxdown.zerx.dev/api/components";

/// 版本列表专用：优先经官网镜像 `MIRROR_BASE/<component>` 拉取（返回原样
/// GitHub JSON），任何失败都回退直连 `github_url`。二进制下载不走此路径。
#[cfg(feature = "components")]
pub(crate) async fn fetch_versions_json(
    client: &reqwest::Client,
    component: &str,
    github_url: &str,
) -> Result<serde_json::Value, ComponentError> {
    let mirror = format!("{MIRROR_BASE}/{component}");
    match fetch_github_json(client, &mirror).await {
        Ok(v) => Ok(v),
        Err(_) => fetch_github_json(client, github_url).await,
    }
}

/// 流式下载 `url` 到 `dest`，`progress(downloaded, total)` 上报进度（total=0 未知）。
/// ffmpeg 归档 / yt-dlp 单二进制安装共用；每 256KB 上报一次避免信号风暴。
#[cfg(feature = "components")]
pub(crate) async fn download_to_file(
    client: &reqwest::Client,
    url: &str,
    dest: &std::path::Path,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<(), ComponentError> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let resp = client
        .get(url)
        .header("User-Agent", "FluxDown")
        .send()
        .await
        .map_err(|e| ComponentError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(ComponentError::Http(format!(
            "download returned {}",
            resp.status()
        )));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| ComponentError::Io(e.to_string()))?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_report: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ComponentError::Http(e.to_string()))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| ComponentError::Io(e.to_string()))?;
        downloaded += chunk.len() as u64;
        if downloaded - last_report >= 256 * 1024 || downloaded == total {
            progress(downloaded, total);
            last_report = downloaded;
        }
    }
    file.flush()
        .await
        .map_err(|e| ComponentError::Io(e.to_string()))?;
    progress(downloaded, total);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::ComponentSource;

    #[test]
    fn source_wire_strings() {
        assert_eq!(ComponentSource::Manual.as_str(), "manual");
        assert_eq!(ComponentSource::Managed.as_str(), "managed");
        assert_eq!(ComponentSource::System.as_str(), "system");
        assert_eq!(ComponentSource::None.as_str(), "none");
    }
}
