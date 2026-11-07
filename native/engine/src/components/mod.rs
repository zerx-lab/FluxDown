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
#[cfg(feature = "components")]
mod segmented;
mod ytdlp;

pub use ffmpeg::*;
pub use ytdlp::*;

use std::path::PathBuf;

/// 组件下载镜像基址的 config 键（675#1）：GitHub Release API 未认证匿名限流
/// 60 请求/小时，公司网络/部分地区直连 `github.com`/`api.github.com` 也可能
/// 被拦截（403/超时）。用户可在设置里填一个 GitHub 反代地址（如自建
/// `ghproxy`/`gh-proxy` 类反代的基址），非空时替换掉请求 URL 里的
/// `https://api.github.com` 或 `https://github.com` 前缀（含 Release JSON
/// 查询与资产直链下载两条路径）。空值（默认）= 不改写，直连 GitHub。
pub const CONFIG_COMPONENT_MIRROR_BASE: &str = "component_mirror_base";

/// 把 `url` 的 `https://api.github.com` / `https://github.com` 前缀替换为
/// `mirror_base`（trim 首尾空白与末尾 `/`）。`mirror_base` 为空则原样返回。
/// 纯函数，不做网络/IO，便于测试。
#[cfg(any(feature = "components", test))]
pub(crate) fn apply_component_mirror(url: &str, mirror_base: &str) -> String {
    let mirror_base = mirror_base.trim().trim_end_matches('/');
    if mirror_base.is_empty() {
        return url.to_string();
    }
    for prefix in ["https://api.github.com", "https://github.com"] {
        if let Some(rest) = url.strip_prefix(prefix) {
            return format!("{mirror_base}{rest}");
        }
    }
    url.to_string()
}

/// 读取用户配置的组件镜像基址（[`CONFIG_COMPONENT_MIRROR_BASE`]）；未设置
/// 或读取失败均返回空串（= 不改写，直连 GitHub），不让配置读取失败阻断
/// 安装流程。
#[cfg(feature = "components")]
pub(crate) async fn component_mirror_base(db: &crate::db::Db) -> String {
    db.get_config(CONFIG_COMPONENT_MIRROR_BASE)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// 组件生效路径的来源。ffmpeg / yt-dlp 共用；`as_str` 为稳定 wire 字符串
/// （各宿主 API 与 UI 徽章共用）。
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
    /// 稳定的 wire 字符串（各宿主 API 共用）。
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

/// macOS/Linux 上 Homebrew（及部分发行版自装软件）常见的固定安装目录。
/// GUI 应用进程继承的 PATH 往往不含这些目录（Finder/启动台启动时 shell
/// 登录脚本未执行），仅在 PATH 扫描失败后作为兜底，不改变 PATH 内的优先级。
#[cfg(target_os = "macos")]
const FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];
#[cfg(all(unix, not(target_os = "macos")))]
const FALLBACK_DIRS: &[&str] = &["/usr/local/bin"];
#[cfg(not(unix))]
const FALLBACK_DIRS: &[&str] = &[];

/// 扫描系统 PATH 寻找指定可执行文件（含 Windows 的 `.exe` 后缀由调用方带入）。
///
/// PATH 扫描失败时，在类 Unix 平台回退探测 [`FALLBACK_DIRS`]（Homebrew 等
/// 固定安装目录），不改变 PATH 内目录的优先级。
pub fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH");
    let path_dirs = path_var
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten();
    let fallback = FALLBACK_DIRS.iter().map(PathBuf::from);
    find_in_dirs(path_dirs.chain(fallback), binary_name)
}

/// 按给定顺序在目录列表中查找第一个存在的 `dir/binary_name` 常规文件；
/// 空目录项跳过。纯函数，不读环境变量，便于测试。
fn find_in_dirs(dirs: impl IntoIterator<Item = PathBuf>, binary_name: &str) -> Option<PathBuf> {
    dirs.into_iter()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(binary_name))
        .find(|candidate| candidate.is_file())
}

/// [`exec_probe`] 的失败原因。
#[derive(thiserror::Error, Debug)]
pub enum ExecProbeError {
    /// 进程无法启动：无执行权限、noexec 挂载、隔离属性 / 安全软件拦截、文件损坏或架构不符。
    #[error("spawn failed: {0}")]
    Spawn(#[source] std::io::Error),
    /// 进程启动了但以非零状态退出；`stderr` 为末尾一行摘要。
    #[error("exited with {status}: {stderr}")]
    Exit { status: String, stderr: String },
    /// 进程成功退出但 stdout 首行为空。
    #[error("no output")]
    NoOutput,
    #[error("timed out after {0:?}")]
    Timeout(std::time::Duration),
}

impl ExecProbeError {
    /// 启动被系统以权限理由拒绝（`EACCES` / `EPERM` / `ERROR_ACCESS_DENIED`）。
    #[must_use]
    pub fn permission_denied(&self) -> bool {
        matches!(self, Self::Spawn(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
    }
}

/// stderr 摘要保留的最大字符数。
const EXEC_PROBE_STDERR_CHARS: usize = 300;

/// 运行 `<path> <arg>`（组件版本探测）并返回 stdout 首行（去首尾空白）。
///
/// `timeout` 为 `None` 时不限时；超时会结束子进程。失败保留根因，供 Doctor 区分
/// 「没有执行权限」与「能启动但运行出错」。
pub async fn exec_probe(
    path: &std::path::Path,
    arg: &str,
    timeout: Option<std::time::Duration>,
) -> Result<String, ExecProbeError> {
    let mut cmd = tokio::process::Command::new(path);
    crate::proc::no_console_window(&mut cmd);
    cmd.arg(arg)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = cmd.output();
    let output = match timeout {
        Some(limit) => tokio::time::timeout(limit, output)
            .await
            .map_err(|_| ExecProbeError::Timeout(limit))?,
        None => output.await,
    }
    .map_err(ExecProbeError::Spawn)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let summary = stderr
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or_default()
            .chars()
            .take(EXEC_PROBE_STDERR_CHARS)
            .collect();
        return Err(ExecProbeError::Exit {
            status: output.status.to_string(),
            stderr: summary,
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match stdout.lines().next().map(str::trim) {
        Some(line) if !line.is_empty() => Ok(line.to_owned()),
        _ => Err(ExecProbeError::NoOutput),
    }
}

/// GitHub Release API JSON 拉取（带 `User-Agent`/`Accept` 头）。ffmpeg / yt-dlp
/// 安装流程共用。
#[cfg(feature = "components")]
pub(crate) async fn fetch_github_json(
    clients: &crate::egress::RoutedClients,
    url: &str,
) -> Result<serde_json::Value, ComponentError> {
    let client = client_for(clients, url).await?;
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

/// [`fetch_github_json`]，但先按用户配置的 [`CONFIG_COMPONENT_MIRROR_BASE`]
/// 改写 `url`（675#1）：改写后请求失败（或未配置，改写后与原 URL 相同，或镜像
/// 不是 https）都回退直连原始 `url`，不让一次镜像故障拖垮整个安装流程。
#[cfg(feature = "components")]
pub(crate) async fn fetch_github_json_with_mirror(
    clients: &crate::egress::RoutedClients,
    url: &str,
    mirror_base: &str,
) -> Result<serde_json::Value, ComponentError> {
    if let Some(mirrored) = https_mirror_url(url, mirror_base)
        && let Ok(v) = fetch_github_json(clients, &mirrored).await
    {
        return Ok(v);
    }
    fetch_github_json(clients, url).await
}

/// 镜像改写后的 URL：仅当确实改写且镜像为 https 才返回；http 镜像一律忽略
/// （退回直连），避免明文链路上的篡改/降级。
#[cfg(any(feature = "components", test))]
pub(crate) fn https_mirror_url(url: &str, mirror_base: &str) -> Option<String> {
    let mirrored = apply_component_mirror(url, mirror_base);
    (mirrored != url && mirrored.to_ascii_lowercase().starts_with("https://")).then_some(mirrored)
}

/// 要求资产/校验文件 URL 落在上游发布仓库的 release 下载前缀内（https、规范化后
/// 仍以 `prefix` 开头）。版本 JSON 可能来自镜像，资产 URL 不可信：即便主机同为
/// github.com，也不能指向别人的仓库。
#[cfg(any(feature = "components", test))]
pub(crate) fn require_upstream_asset_url(url: &str, prefix: &str) -> Result<(), ComponentError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ComponentError::Verify(format!("invalid asset url: {e}")))?;
    let ok = parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.as_str().starts_with(prefix);
    if ok {
        Ok(())
    } else {
        Err(ComponentError::Verify(format!(
            "asset url is not under the upstream release path: {url}"
        )))
    }
}

/// 解析 `sha256sum` 风格校验文件（`<hex>  <name>` / `<hex> *<name>`），取 `name`
/// 对应的 64 位十六进制摘要（小写）。
#[cfg(any(feature = "components", test))]
pub(crate) fn parse_checksum_file(text: &str, asset_name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let hash = it.next()?;
        let name = it.next()?.trim_start_matches('*');
        (name == asset_name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// 校验文件体积上限（真实文件 < 10KB）。
#[cfg(feature = "components")]
const MAX_CHECKSUM_FILE_BYTES: usize = 1024 * 1024;

/// 取上游公布的 SHA-256：先直连上游校验文件；直连失败才经用户配置的 https
/// 镜像取（受限网络用户的唯一通路，信任等同于他自己选的镜像）。官网版本镜像
/// 不参与——它只转发版本 JSON，不在校验文件的信任路径上。
#[cfg(feature = "components")]
pub(crate) async fn fetch_expected_sha256(
    clients: &crate::egress::RoutedClients,
    checksum_url: &str,
    mirror_base: &str,
    asset_name: &str,
) -> Result<String, ComponentError> {
    let mut urls = vec![checksum_url.to_string()];
    urls.extend(https_mirror_url(checksum_url, mirror_base));
    let mut last = ComponentError::NotFound(format!("checksum for {asset_name}"));
    for url in urls {
        match fetch_text(clients, &url).await {
            Ok(text) => match parse_checksum_file(&text, asset_name) {
                Some(hash) => return Ok(hash),
                None => {
                    last = ComponentError::NotFound(format!("checksum entry for {asset_name}"));
                }
            },
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(feature = "components")]
async fn fetch_text(
    clients: &crate::egress::RoutedClients,
    url: &str,
) -> Result<String, ComponentError> {
    let client = client_for(clients, url).await?;
    let resp = client
        .get(url)
        .header("User-Agent", "FluxDown")
        .send()
        .await
        .map_err(|e| ComponentError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(ComponentError::Http(format!(
            "checksum download returned {}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ComponentError::Http(e.to_string()))?;
    if bytes.len() > MAX_CHECKSUM_FILE_BYTES {
        return Err(ComponentError::Http("checksum file too large".to_string()));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 官网组件版本镜像的基地址。版本列表拉取优先经此转发（服务端持 token +
/// 24h 缓存，规避 GitHub 匿名 API 每 IP 60/h 限流与直连 api.github.com 的
/// 网络问题），失败依次回退用户配置的镜像（若有）与直连 GitHub。
#[cfg(feature = "components")]
const MIRROR_BASE: &str = "https://fluxdown.com/api/components";

/// 版本列表专用：优先经官网镜像 `MIRROR_BASE/<component>` 拉取（返回原样
/// GitHub JSON）；失败则经 [`fetch_github_json_with_mirror`] 依次尝试用户
/// 配置的镜像与直连 `github_url`。二进制下载不走此路径。
#[cfg(feature = "components")]
pub(crate) async fn fetch_versions_json(
    clients: &crate::egress::RoutedClients,
    component: &str,
    github_url: &str,
    mirror_base: &str,
) -> Result<serde_json::Value, ComponentError> {
    let official_mirror = format!("{MIRROR_BASE}/{component}");
    if let Ok(v) = fetch_github_json(clients, &official_mirror).await {
        return Ok(v);
    }
    fetch_github_json_with_mirror(clients, github_url, mirror_base).await
}

/// 下载载荷到 `dest` 并校验 SHA-256（`expected_sha256`，小写 hex），
/// `progress(downloaded, total)` 上报进度（total=0 未知）。ffmpeg 归档 / yt-dlp
/// 单二进制安装共用；每 256KB 上报一次避免信号风暴。服务器支持 Range 时多段并发、
/// 单段断点重试，否则单流（见 [`segmented`]，#453）。
///
/// 依次尝试：用户配置的 [`CONFIG_COMPONENT_MIRROR_BASE`] 镜像（675#1，仅 https）→
/// 统一出口的插件改写地址（[`crate::egress::Egress::route_verified`]，如 GitHub 加速）→
/// 原始上游地址。任一来源下载失败或内容哈希不符都换下一个；最后一个仍不符则删除 `dest`
/// 并报错——调用方在校验通过前不得执行或落位该文件。
#[cfg(feature = "components")]
pub(crate) async fn download_to_file(
    clients: &crate::egress::RoutedClients,
    route: &crate::egress::EgressRoute,
    mirror_base: &str,
    dest: &std::path::Path,
    expected_sha256: &str,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<(), ComponentError> {
    let mut attempts = Vec::new();
    if let Some(mirrored) = https_mirror_url(route.source_url(), mirror_base) {
        attempts.push(crate::egress::EgressAttempt {
            url: mirrored,
            headers: Vec::new(),
            plugin: None,
        });
    }
    attempts.extend(route.attempts());
    let mut last_error = ComponentError::NotFound("download source".to_string());
    for attempt in &attempts {
        match download_to_file_from(clients, attempt, dest, progress).await {
            Ok(actual) if actual.eq_ignore_ascii_case(expected_sha256) => return Ok(()),
            Ok(actual) => {
                crate::log_info!(
                    "[components] content hash mismatch from {} source, trying next",
                    attempt.plugin.as_deref().unwrap_or("mirror/upstream")
                );
                last_error = ComponentError::Verify(format!(
                    "sha256 mismatch for downloaded component (expected {expected_sha256}, got {actual})"
                ));
            }
            Err(error) => {
                crate::log_info!(
                    "[components] download from {} source failed: {error}",
                    attempt.plugin.as_deref().unwrap_or("mirror/upstream")
                );
                last_error = error;
            }
        }
    }
    if let Err(error) = tokio::fs::remove_file(dest).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        crate::logger::report_warning("components", "remove_unverified_download", &error);
    }
    Err(last_error)
}

/// 下载到 `dest`，返回内容的 SHA-256（小写 hex）。
#[cfg(feature = "components")]
async fn download_to_file_from(
    clients: &crate::egress::RoutedClients,
    attempt: &crate::egress::EgressAttempt,
    dest: &std::path::Path,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<String, ComponentError> {
    let client = client_for(clients, &attempt.url).await?;
    segmented::fetch_to_file(&client, attempt, dest, progress, &segmented::SEGMENT_POLICY).await
}

/// 组件安装的 HTTP 客户端集合：统一出口按当前全局代理与站点规则装配。
#[cfg(feature = "components")]
pub(crate) async fn egress_clients(
    egress: &crate::egress::Egress,
) -> Result<crate::egress::RoutedClients, ComponentError> {
    egress
        .routed(|builder| builder)
        .await
        .map_err(|e| ComponentError::Http(format!("{e:#}")))
}

/// `url` 命中站点规则时用规则出口的客户端，否则用全局出口的客户端。
#[cfg(feature = "components")]
async fn client_for(
    clients: &crate::egress::RoutedClients,
    url: &str,
) -> Result<reqwest::Client, ComponentError> {
    clients
        .client(url)
        .await
        .map_err(|e| ComponentError::Http(format!("{e:#}")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::{
        ComponentSource, apply_component_mirror, https_mirror_url, parse_checksum_file,
        require_upstream_asset_url,
    };

    #[test]
    fn source_wire_strings() {
        assert_eq!(ComponentSource::Manual.as_str(), "manual");
        assert_eq!(ComponentSource::Managed.as_str(), "managed");
        assert_eq!(ComponentSource::System.as_str(), "system");
        assert_eq!(ComponentSource::None.as_str(), "none");
    }

    /// 目录顺序即优先级：前面的目录命中即返回，空目录项被跳过，缺失目录
    /// 不影响后续（模拟 PATH 未命中后回退到 Homebrew 目录）。
    #[test]
    fn find_in_dirs_respects_order_and_skips_empty_or_missing() {
        let root = std::env::temp_dir().join(format!(
            "fluxdown-find-in-dirs-{}-{}",
            std::process::id(),
            line!()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let name = "fluxdown-test-binary";
        std::fs::write(second.join(name), b"").unwrap();
        // `first` 里只有同名目录，不是文件，不应命中。
        std::fs::create_dir_all(first.join(name)).unwrap();

        let dirs = vec![
            PathBuf::new(),
            root.join("missing"),
            first.clone(),
            second.clone(),
        ];
        let found = super::find_in_dirs(dirs, name);
        let none = super::find_in_dirs([first, second], "fluxdown-definitely-not-installed");
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(found, Some(root.join("second").join(name)));
        assert_eq!(none, None);
    }

    // 675#1：未配置镜像时原样返回。
    #[test]
    fn apply_component_mirror_noop_when_unset() {
        let url = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
        assert_eq!(apply_component_mirror(url, ""), url);
        assert_eq!(apply_component_mirror(url, "   "), url);
    }

    // 配置后改写 api.github.com 前缀（Release JSON 端点）。
    #[test]
    fn apply_component_mirror_rewrites_api_host() {
        let url = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
        assert_eq!(
            apply_component_mirror(url, "https://ghproxy.example.com/gh"),
            "https://ghproxy.example.com/gh/repos/yt-dlp/yt-dlp/releases/latest"
        );
    }

    // 配置后改写 github.com 前缀（资产直链下载）；末尾 `/` 与首尾空白容错。
    #[test]
    fn apply_component_mirror_rewrites_asset_download_host() {
        let url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg.zip";
        assert_eq!(
            apply_component_mirror(url, "  https://ghproxy.example.com/gh/  "),
            "https://ghproxy.example.com/gh/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg.zip"
        );
    }

    #[test]
    fn mirror_must_be_https_to_be_used() {
        let url = "https://github.com/yt-dlp/yt-dlp/releases/download/2026.01.01/yt-dlp";
        assert_eq!(https_mirror_url(url, ""), None);
        assert_eq!(https_mirror_url(url, "http://proxy.example.com/gh"), None);
        assert_eq!(
            https_mirror_url(url, "https://proxy.example.com/gh").as_deref(),
            Some("https://proxy.example.com/gh/yt-dlp/yt-dlp/releases/download/2026.01.01/yt-dlp")
        );
        // 与原 URL 无关的地址不产生改写。
        assert_eq!(
            https_mirror_url("https://example.org/x", "https://proxy.example.com/gh"),
            None
        );
    }

    #[test]
    fn asset_url_must_stay_in_upstream_release_path() {
        let prefix = "https://github.com/yt-dlp/yt-dlp/releases/download/";
        assert!(
            require_upstream_asset_url(
                "https://github.com/yt-dlp/yt-dlp/releases/download/2026.01.01/yt-dlp",
                prefix
            )
            .is_ok()
        );
        for bad in [
            "http://github.com/yt-dlp/yt-dlp/releases/download/1/yt-dlp",
            "https://github.com/evil/repo/releases/download/1/yt-dlp",
            "https://evil.example.com/yt-dlp/yt-dlp/releases/download/1/yt-dlp",
            "https://github.com/yt-dlp/yt-dlp/releases/download/../../../evil/x/releases/download/1/a",
            "https://user@github.com/yt-dlp/yt-dlp/releases/download/1/yt-dlp",
            "not a url",
        ] {
            assert!(
                require_upstream_asset_url(bad, prefix).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn checksum_file_lookup_matches_exact_asset_name() {
        let h1 = "a".repeat(64);
        let h2 = "B".repeat(64);
        let text = format!("{h1}  yt-dlp\n{h2} *yt-dlp_macos\nshort  yt-dlp_x\n");
        assert_eq!(parse_checksum_file(&text, "yt-dlp"), Some(h1));
        assert_eq!(
            parse_checksum_file(&text, "yt-dlp_macos"),
            Some("b".repeat(64))
        );
        // 前缀相同的名字不可串；摘要长度不对的行忽略。
        assert_eq!(parse_checksum_file(&text, "yt-dlp_"), None);
        assert_eq!(parse_checksum_file(&text, "yt-dlp_x"), None);
    }

    // 不匹配已知前缀的 URL 原样透传（不误伤第三方镜像/官网 URL）。
    #[test]
    fn apply_component_mirror_leaves_unrelated_urls_untouched() {
        let url = "https://fluxdown.com/api/components/ffmpeg";
        assert_eq!(
            apply_component_mirror(url, "https://ghproxy.example.com/gh"),
            url
        );
    }
}
