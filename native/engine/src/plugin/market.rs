//! 去中心化插件市场客户端（R4/R5/R6）。
//!
//! 市场不是网站，而是**一份可验证的数据格式**：Git 版本化的索引（联邦式，任何人可
//! fork 另立），插件包内容寻址（`content_hash = sha256(整个 .fxplug zip)`），经多源
//! （CDN / R2 / 众包镜像）分发。**FluxDown 用自己的引擎下载并本地验证插件**。
//!
//! ## v1 范围（记录在案的取舍）
//! - 完整性基座 = `content_hash`（sha256，已在依赖树）+ Git Merkle DAG 防篡改 +
//!   per-index_id sequence 单调高水位防回滚。
//! - **作者签名（sigstore / ed25519）推迟**：受依赖约束（仅 rquickjs 获批），不引入
//!   sigstore-rs / ed25519 crate。索引 schema 预留 `sig_scheme`/`sigstore_bundle_ref`
//!   字段，晚加不破坏兼容（JSON）。当前信任模型 = 内容寻址 + 传输层 TLS + Git 历史。
//! - `.fxplug` = 插件目录（manifest.json + *.js）的 zip；安装复用
//!   [`super::install::install_from_zip`]（zip-slip / 压缩炸弹防护 + manifest 校验）。
//! - 包内 manifest 是**不可信声明**：授权界面展示的是索引条目的 `permissions`，故
//!   安装前（替换旧版本之前）强制核对 manifest 的 identity / version 与条目完全相等、
//!   permissions 为条目声明的子集，否则拒绝（第三方 fork 索引不可信）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::Db;
use crate::logger::{log_warn, sanitize_log_str};
use crate::proxy_config::ProxyConfig;

use super::manager::PluginManager;
use super::manifest::PluginManifest;
use super::runtime::PluginError;

/// 内置官方索引候选源（不同法域/托管，共享同一 index_id；任一存活即可用）。
/// 首个成功者胜出；用户可经配置覆盖/追加。
pub const DEFAULT_INDEX_SOURCES: &[&str] = &[
    "https://raw.githubusercontent.com/zerx-lab/fluxdown-plugin-index/main/index.json",
    "https://cdn.jsdelivr.net/gh/zerx-lab/fluxdown-plugin-index@main/index.json",
];

/// 下载/校验/安装的错误。
#[derive(Debug, thiserror::Error)]
pub enum MarketError {
    #[error("网络错误: {0}")]
    Network(String),
    #[error("索引解析失败: {0}")]
    IndexParse(String),
    #[error("内容哈希不匹配: 期望 {expected}，实际 {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("sequence 回退（可能的回滚攻击）: 索引 {seen} < 本地高水位 {watermark}")]
    SequenceRollback { seen: u64, watermark: u64 },
    #[error("插件包超过体积上限")]
    TooLarge,
    #[error("索引响应超过体积上限")]
    IndexTooLarge,
    #[error("未找到插件条目: {0}")]
    NotFound(String),
    #[error("插件包被 yanked（{0}），需显式确认安装")]
    Yanked(String),
    #[error("所有镜像下载失败")]
    AllMirrorsFailed,
    #[error("市场最新版本已变化: 确认的是 {expected}，当前为 {actual}")]
    VersionChanged { expected: String, actual: String },
    #[error("插件 {0} 由 FluxCloud 私有商店安装，公开市场不能覆盖它")]
    PrivateSource(String),
    #[error(transparent)]
    Plugin(#[from] PluginError),
}

/// 索引条目（每插件每版本一份分片，flatten 进 `index.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketEntry {
    pub plugin_id: String,
    pub version: String,
    /// 全索引全局单调递增整数（防回滚基线）。
    pub sequence: u64,
    /// `sha256:<hex>` —— 唯一真相源，下载后钉住比对。
    pub content_hash: String,
    #[serde(default)]
    pub min_app_version: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: String,
    /// 多源下载 URL（https 白名单；逐个尝试直到 content_hash 吻合）。
    #[serde(default)]
    pub mirrors: Vec<String>,
    #[serde(default)]
    pub publish_time: String,
    /// `none` / `deprecated` / `vulnerable` / `malicious`。
    #[serde(default = "yanked_none")]
    pub yanked: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// manifest 声明的能力权限（如 `["ffmpeg"]`；CI flatten 自 manifest 抄录，
    /// 供安装前展示授权。旧索引缺省 → 空，不影响安装）。
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 插件桌面视图声明的网络主机；授权界面展示它，包内 manifest 须为其子集。
    #[serde(default)]
    pub network_hosts: Vec<String>,
    /// 预留：未来作者签名方案（`none` / `sigstore` / `ed25519`）。
    #[serde(default = "sig_none")]
    pub sig_scheme: String,
    #[serde(default)]
    pub sigstore_bundle_ref: String,
}

fn yanked_none() -> String {
    "none".to_string()
}
fn sig_none() -> String {
    "none".to_string()
}

/// 索引根（`index.json`，CI flatten 生成）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketIndex {
    /// 索引源身份（CI 初始化时一次性生成的 UUID；fork 保留 → 共享 sequence 高水位）。
    pub index_id: String,
    /// 全索引 sequence 高水位。
    pub sequence: u64,
    #[serde(default)]
    pub updated: String,
    /// 全部条目（每插件可多版本）。
    pub entries: Vec<MarketEntry>,
}

/// 插件包体积上限（10MB，与管理 API 安装上限一致）。
const MAX_FXPLUG_BYTES: usize = 10 * 1024 * 1024;
/// 索引 JSON 体积上限（流式截断防 OOM；真实索引远小于此，被投毒/损坏的源
/// 可能返回任意大响应，`.text()` 全量缓冲会被撑爆）。
const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;
/// 包下载最多跟随的重定向跳数。
const MAX_DOWNLOAD_REDIRECTS: usize = 5;

/// 市场客户端。持有插件管理器（安装）与 Db（高水位持久化）。
pub struct MarketClient {
    manager: std::sync::Arc<PluginManager>,
    db: Db,
    clients: crate::egress::RoutedClients,
    /// 包下载专用：重定向逐跳复查 https 与字面量 IP 守卫（初始 URL 的
    /// `mirror_url_allowed` 只管第一跳）。
    download_clients: crate::egress::RoutedClients,
    /// 宿主自身下载的统一出口：包（content_hash 钉住）可经插件改写来源（如 GitHub 加速）。
    egress: crate::egress::Egress,
    sources: Vec<String>,
}

impl MarketClient {
    /// 构造。空 `sources` 使用内置源；代理与下载客户端共享装配规则，构造失败返回错误。
    pub fn new(
        manager: std::sync::Arc<PluginManager>,
        db: Db,
        sources: Vec<String>,
        proxy: &ProxyConfig,
    ) -> Result<Self, MarketError> {
        let sources = if sources.is_empty() {
            DEFAULT_INDEX_SOURCES
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            sources
        };
        let clients = crate::egress::RoutedClients::new(proxy, "", |b| {
            b.timeout(std::time::Duration::from_secs(20))
        })
        .map_err(client_build_error)?;
        let download_clients = crate::egress::RoutedClients::new(proxy, "", |b| {
            b.timeout(std::time::Duration::from_secs(20)).redirect(
                reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= MAX_DOWNLOAD_REDIRECTS {
                        return attempt.error("too many redirects");
                    }
                    if !parsed_url_allowed(attempt.url()) {
                        let message = format!(
                            "redirect target rejected by mirror guard: {}",
                            safe_url(attempt.url().as_str())
                        );
                        return attempt.error(message);
                    }
                    attempt.follow()
                }),
            )
        })
        .map_err(client_build_error)?;
        let egress = crate::egress::Egress::new(db.clone()).with_plugins(manager.clone());
        Ok(Self {
            manager,
            db,
            clients,
            download_clients,
            egress,
            sources,
        })
    }

    /// 拉取索引（逐源 failover，首个成功者胜出）。校验 sequence 不回退（防回滚）。
    pub async fn fetch_index(&self) -> Result<MarketIndex, MarketError> {
        let mut last_err = MarketError::AllMirrorsFailed;
        for src in &self.sources {
            match self.fetch_index_from(src).await {
                Ok(idx) => {
                    if let Err(error) = self.check_and_update_watermark(&idx).await {
                        log_warn!(
                            "[market] index source={} rejected: {error:#}",
                            safe_url(src)
                        );
                        return Err(error);
                    }
                    return Ok(idx);
                }
                Err(error) => {
                    log_warn!(
                        "[market] index source={} failed, trying next source: {error:#}",
                        safe_url(src)
                    );
                    last_err = error;
                }
            }
        }
        Err(last_err)
    }

    async fn fetch_index_from(&self, url: &str) -> Result<MarketIndex, MarketError> {
        let client = self.clients.client(url).await.map_err(client_build_error)?;
        let mut resp = client.get(url).send().await.map_err(network_error)?;
        if !resp.status().is_success() {
            return Err(MarketError::Network(format!(
                "HTTP {} at {}",
                resp.status(),
                safe_url(resp.url().as_str())
            )));
        }
        // 流式读取 + 体积上限（与 download_one 对称，防恶意源 OOM）。
        let mut buf = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if buf.len() + chunk.len() > MAX_INDEX_BYTES {
                        return Err(MarketError::IndexTooLarge);
                    }
                    buf.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(error) => return Err(network_error(error)),
            }
        }
        serde_json::from_slice::<MarketIndex>(&buf)
            .map_err(|e| MarketError::IndexParse(e.to_string()))
    }

    /// per-index_id 高水位防回滚：索引 sequence 不得低于本地已见最高值。
    async fn check_and_update_watermark(&self, idx: &MarketIndex) -> Result<(), MarketError> {
        let key = format!("market.{}.sequence", idx.index_id);
        let watermark: u64 = self
            .db
            .get_config(&key)
            .await
            .map_err(|e| PluginError::Runtime(format!("读取市场高水位失败: {e:#}")))?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if idx.sequence < watermark {
            return Err(MarketError::SequenceRollback {
                seen: idx.sequence,
                watermark,
            });
        }
        if idx.sequence > watermark {
            self.db
                .set_config(&key, &idx.sequence.to_string())
                .await
                .map_err(|e| PluginError::Runtime(format!("写入市场高水位失败: {e:#}")))?;
        }
        Ok(())
    }

    /// 找到某 plugin_id 的最新非 yanked 版本条目。
    pub fn latest_entry<'a>(
        &self,
        idx: &'a MarketIndex,
        plugin_id: &str,
    ) -> Option<&'a MarketEntry> {
        idx.entries
            .iter()
            .filter(|e| e.plugin_id == plugin_id && e.yanked == "none")
            .max_by(|a, b| a.sequence.cmp(&b.sequence))
    }

    /// 安装指定条目：多镜像择优下载 → content_hash 钉住校验 → install_from_zip。
    /// `allow_yanked = false` 时拒绝 yanked 版本（malicious 恒拒）。
    pub async fn install_entry(
        &self,
        entry: &MarketEntry,
        allow_yanked: bool,
    ) -> Result<String, MarketError> {
        if self.manager.is_private_source(&entry.plugin_id).await? {
            return Err(MarketError::PrivateSource(entry.plugin_id.clone()));
        }
        if entry.yanked == "malicious" {
            return Err(MarketError::Yanked("malicious".to_string()));
        }
        if entry.yanked != "none" && !allow_yanked {
            return Err(MarketError::Yanked(entry.yanked.clone()));
        }
        let bytes = self.download_verified(entry).await?;
        // 联网前的快速拒绝在上面；这里把「公开路径不得覆盖私有来源」下沉到安装锁内重查，
        // 关闭检查与落盘之间的窗口。
        let identity = self
            .manager
            .install_from_zip_checked(bytes, true, |manifest| {
                verify_package_claim(entry, manifest)
            })
            .await
            .map_err(|error| match error {
                PluginError::PrivateSource(id) => MarketError::PrivateSource(id),
                other => MarketError::Plugin(other),
            })?;
        Ok(identity)
    }

    /// 逐镜像下载并校验 content_hash（首个吻合者胜出）。https 白名单。
    async fn download_verified(&self, entry: &MarketEntry) -> Result<Vec<u8>, MarketError> {
        let expected = entry
            .content_hash
            .strip_prefix("sha256:")
            .unwrap_or(&entry.content_hash)
            .to_ascii_lowercase();
        // 下载错误不能盖掉已确认的内容错误；所有镜像仍继续尝试。
        let mut verification_error = None;
        for url in &entry.mirrors {
            // 镜像白名单：https-only（防降级）+ 字面量 IP 必须可全局路由（联邦
            // 索引的镜像 URL 不可全信，挡「把环回/内网地址伪装成镜像」的 SSRF
            // 探测；hostname 级过滤不做——自托管 LAN 索引经 hostname 仍可用，
            // 记录在案的 v1 取舍，完整性由 content_hash 钉住兜底）。
            if !mirror_url_allowed(url) {
                log_warn!(
                    "[market] plugin={:?} version={:?} mirror={} rejected by URL guard (HTTPS and globally routable literal IP required)",
                    entry.plugin_id,
                    entry.version,
                    safe_url(url)
                );
                continue;
            }
            // 插件改写（如 GitHub 加速）只换来源，内容仍由 content_hash 钉住；改写地址
            // 同样过镜像守卫，失败或内容不符回退原镜像地址。索引不经插件（它是完整性锚）。
            let route = self.egress.route_verified(url).await;
            for attempt in route.attempts() {
                if !mirror_url_allowed(&attempt.url) {
                    log_warn!(
                        "[market] plugin={:?} version={:?} source={} rewritten by {:?} rejected by URL guard",
                        entry.plugin_id,
                        entry.version,
                        safe_url(&attempt.url),
                        attempt.plugin
                    );
                    continue;
                }
                let error = match self.download_one(&attempt.url, &attempt.headers).await {
                    Ok(bytes) => {
                        let actual = sha256_hex(&bytes);
                        if actual == expected {
                            return Ok(bytes);
                        }
                        MarketError::HashMismatch {
                            expected: expected.clone(),
                            actual,
                        }
                    }
                    Err(error) => error,
                };
                log_warn!(
                    "[market] plugin={:?} version={:?} source={} failed, trying next source: {error:#}",
                    entry.plugin_id,
                    entry.version,
                    safe_url(&attempt.url)
                );
                if matches!(
                    error,
                    MarketError::HashMismatch { .. } | MarketError::TooLarge
                ) {
                    verification_error = Some(error);
                }
            }
        }
        let error = verification_error.unwrap_or(MarketError::AllMirrorsFailed);
        log_warn!(
            "[market] plugin={:?} version={:?} all {} mirrors failed: {error:#}",
            entry.plugin_id,
            entry.version,
            entry.mirrors.len()
        );
        Err(error)
    }

    async fn download_one(
        &self,
        url: &str,
        headers: &[(String, String)],
    ) -> Result<Vec<u8>, MarketError> {
        let client = self
            .download_clients
            .client(url)
            .await
            .map_err(client_build_error)?;
        let mut request = client.get(url);
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let resp = request.send().await.map_err(network_error)?;
        if !resp.status().is_success() {
            return Err(MarketError::Network(format!(
                "HTTP {} at {}",
                resp.status(),
                safe_url(resp.url().as_str())
            )));
        }
        let mut stream_resp = resp;
        let mut buf = Vec::new();
        loop {
            match stream_resp.chunk().await {
                Ok(Some(chunk)) => {
                    if buf.len() + chunk.len() > MAX_FXPLUG_BYTES {
                        log_warn!(
                            "[market] package source={} exceeds size limit: received at least {} bytes, limit {} bytes",
                            safe_url(stream_resp.url().as_str()),
                            buf.len() + chunk.len(),
                            MAX_FXPLUG_BYTES
                        );
                        return Err(MarketError::TooLarge);
                    }
                    buf.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(error) => return Err(network_error(error)),
            }
        }
        Ok(buf)
    }

    /// 便捷：按 plugin_id 安装最新版（拉索引 → 找最新 → 安装）。
    ///
    /// `expected_version` 是调用方（UI 权限确认对话框）展示并确认过的版本：
    /// 与当前最新可装版本不一致时返回 [`MarketError::VersionChanged`] 且**不安装**，
    /// 让调用方刷新目录后重新确认权限，避免「确认的是旧版声明，装上的是新版」。
    pub async fn install_latest(
        &self,
        plugin_id: &str,
        expected_version: Option<&str>,
    ) -> Result<String, MarketError> {
        let idx = self.fetch_index().await?;
        let entry = self
            .latest_entry(&idx, plugin_id)
            .ok_or_else(|| MarketError::NotFound(plugin_id.to_string()))?
            .clone();
        check_expected_version(expected_version, &entry.version)?;
        self.install_entry(&entry, false).await
    }

    /// 市场来源配置（供 UI 展示/编辑）。
    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    /// 当前设置值映射（供上层构造带自定义源的客户端）。
    pub fn source_config(map: &HashMap<String, String>) -> Vec<String> {
        map.get("market_index_sources")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// 安装 FluxCloud 私有商店的包（`daemon.plugin.installPrivate`）：不经 [`MarketClient`]、不碰公开索引
/// 高水位。`bytes` 的 sha256 必须等于条目 `content_hash`，包内 manifest 的 identity / version /
/// 权限 / 网络主机按与公开市场相同的 [`verify_package_claim`] 核对（先于替换旧版本），成功后记录
/// 来源 `private`。非 `yanked == "none"` 的条目一律拒绝。
pub async fn install_private_package(
    manager: &PluginManager,
    bytes: Vec<u8>,
    entry: &MarketEntry,
) -> Result<String, MarketError> {
    if entry.yanked != "none" {
        return Err(MarketError::Yanked(entry.yanked.clone()));
    }
    if bytes.len() > MAX_FXPLUG_BYTES {
        return Err(MarketError::TooLarge);
    }
    let expected = entry
        .content_hash
        .strip_prefix("sha256:")
        .unwrap_or(&entry.content_hash)
        .to_ascii_lowercase();
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(MarketError::HashMismatch { expected, actual });
    }
    let identity = manager
        .install_private_checked(bytes, |manifest| verify_package_claim(entry, manifest))
        .await?;
    Ok(identity)
}

/// 日志仅保留 URL 的 scheme、域名/端口和路径，绝不带凭据/query/fragment。
fn safe_url(raw: &str) -> String {
    let Ok(parsed) = url::Url::parse(raw) else {
        return "<invalid URL>".to_owned();
    };
    if parsed.host_str().is_none() {
        return "<URL without host>".to_owned();
    }
    sanitize_log_str(&format!(
        "{}://{}",
        parsed.scheme(),
        &parsed[url::Position::BeforeHost..url::Position::AfterPath]
    ))
}

fn safe_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    static URLS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)[a-z][a-z0-9+.-]*://[^\s<>()"'{}]+"#)
            .unwrap_or_else(|error| panic!("invalid market diagnostic URL regex: {error}"))
    });
    let chain = crate::logger::format_error_chain(error);
    sanitize_log_str(&URLS.replace_all(&chain, |capture: &regex::Captures<'_>| {
        safe_url(&capture[0])
    }))
}

fn network_error(error: reqwest::Error) -> MarketError {
    MarketError::Network(safe_error_chain(&error.without_url()))
}

fn client_build_error(error: crate::downloader::DownloadError) -> MarketError {
    MarketError::Network(format!(
        "构造市场 HTTP 客户端失败: {}",
        safe_error_chain(&error)
    ))
}

/// 核对已解析校验的包内 manifest 与索引条目：identity、version 必须完全相等，
/// permissions 必须是条目 permissions 的子集（防止越权声明绕过授权界面）。
fn verify_package_claim(entry: &MarketEntry, manifest: &PluginManifest) -> Result<(), PluginError> {
    if manifest.identity != entry.plugin_id {
        return Err(PluginError::ManifestInvalid(format!(
            "插件包 manifest 的 identity '{}' 与索引条目 '{}' 不一致",
            manifest.identity, entry.plugin_id
        )));
    }
    if manifest.version != entry.version {
        return Err(PluginError::ManifestInvalid(format!(
            "插件包 manifest 的 version '{}' 与索引条目 '{}' 不一致",
            manifest.version, entry.version
        )));
    }
    if let Some(extra) = manifest
        .permissions
        .iter()
        .find(|p| !entry.permissions.contains(p))
    {
        return Err(PluginError::ManifestInvalid(format!(
            "插件包 manifest 声明了索引条目未授权的权限 '{extra}'（manifest permissions: {:?}，索引 permissions: {:?}）",
            manifest.permissions, entry.permissions
        )));
    }
    if let Some(extra) = manifest
        .network_hosts()
        .into_iter()
        .find(|h| !entry.network_hosts.contains(h))
    {
        return Err(PluginError::ManifestInvalid(format!(
            "插件包 manifest 声明了索引条目未授权的网络主机 '{extra}'（索引 networkHosts: {:?}）",
            entry.network_hosts
        )));
    }
    Ok(())
}

/// 调用方确认过的版本必须与当前最新可装版本一致；未指定（旧客户端）不校验。
fn check_expected_version(expected: Option<&str>, actual: &str) -> Result<(), MarketError> {
    match expected {
        Some(expected) if expected != actual => Err(MarketError::VersionChanged {
            expected: expected.to_string(),
            actual: actual.to_string(),
        }),
        _ => Ok(()),
    }
}

/// `sha256(bytes)` 的小写 hex。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 镜像 URL 白名单：https-only + 字面量 IP host 必须可全局路由。
///
/// hostname 不做 DNS 级过滤（自托管 LAN 索引经 hostname 仍可用，v1 取舍）；
/// 完整性由 content_hash 钉住兜底，此守卫只挡最直接的内网探测形态。
fn mirror_url_allowed(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    parsed_url_allowed(&parsed)
}

/// 已解析 URL 的守卫（初始镜像与每一跳重定向共用）：https + 字面量 IP 可全局路由。
fn parsed_url_allowed(parsed: &url::Url) -> bool {
    if parsed.scheme() != "https" {
        return false;
    }
    if let Some(host) = parsed.host_str() {
        let trimmed = host.trim_matches(|c| c == '[' || c == ']');
        if let Ok(ip) = trimmed.parse::<std::net::IpAddr>()
            && !super::bridge::is_globally_routable_unicast(ip)
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{
        MarketError, MarketIndex, check_expected_version, mirror_url_allowed, safe_error_chain,
        safe_url, sha256_hex,
    };

    #[test]
    fn diagnostics_keep_causes_but_strip_all_url_secrets() {
        let raw = "https://private-user:private-pass@cdn.example.com:8443/p.fxplug?query-secret#fragment-secret";
        assert_eq!(safe_url(raw), "https://cdn.example.com:8443/p.fxplug");
        assert_eq!(safe_url("malformed-query-secret"), "<invalid URL>");

        #[derive(Debug, thiserror::Error)]
        #[error("request for {url} failed")]
        struct RequestFailure {
            url: String,
            #[source]
            source: std::io::Error,
        }
        let error = RequestFailure {
            url: raw.to_owned(),
            source: std::io::Error::other(
                "TLS certificate root failure at https://nested-user:nested-pass@proxy.example.com/tunnel?nested-query#nested-fragment",
            ),
        };
        let diagnostic = safe_error_chain(&error);
        assert!(diagnostic.contains("TLS certificate root failure"));
        assert!(diagnostic.contains("https://cdn.example.com:8443/p.fxplug"));
        assert!(diagnostic.contains("https://proxy.example.com/tunnel"));
        for secret in [
            "private-user",
            "private-pass",
            "query-secret",
            "fragment-secret",
            "nested-user",
            "nested-pass",
            "nested-query",
            "nested-fragment",
        ] {
            assert!(
                !diagnostic.contains(secret),
                "leaked {secret}: {diagnostic}"
            );
        }
    }

    /// 确认过的版本与最新不一致必须拒绝；一致或未指定放行。
    #[test]
    fn expected_version_pin_rejects_changed_latest() {
        assert!(check_expected_version(Some("1.2.0"), "1.2.0").is_ok());
        assert!(check_expected_version(None, "9.9.9").is_ok());
        let err = check_expected_version(Some("1.2.0"), "1.3.0").expect_err("changed");
        assert!(matches!(
            err,
            MarketError::VersionChanged { ref expected, ref actual }
                if expected == "1.2.0" && actual == "1.3.0"
        ));
    }

    #[test]
    fn mirror_whitelist_rejects_http_and_nonroutable_ip() {
        // https-only。
        assert!(!mirror_url_allowed("http://cdn.example.com/a.fxplug"));
        assert!(!mirror_url_allowed("ftp://cdn.example.com/a.fxplug"));
        // 字面量非公网 IP 拒绝（环回/私网/元数据/v6 环回）。
        assert!(!mirror_url_allowed("https://127.0.0.1/a.fxplug"));
        assert!(!mirror_url_allowed("https://192.168.1.10:8443/a.fxplug"));
        assert!(!mirror_url_allowed("https://169.254.169.254/a.fxplug"));
        assert!(!mirror_url_allowed("https://[::1]/a.fxplug"));
        // 公网字面量 IP 与 hostname 放行。
        assert!(mirror_url_allowed("https://93.184.216.34/a.fxplug"));
        assert!(mirror_url_allowed("https://cdn.jsdelivr.net/gh/x/y.fxplug"));
        assert!(mirror_url_allowed("https://lan-index.internal/a.fxplug"));
    }

    #[test]
    fn sha256_known_vector() {
        // sha256("") = e3b0c442...
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn index_parses_camel_case() {
        let json = r#"{
          "indexId": "00000000-0000-0000-0000-000000000000",
          "sequence": 3,
          "updated": "2026-07-13T00:00:00Z",
          "entries": [{
            "pluginId": "a@b", "version": "1.0.0", "sequence": 3,
            "contentHash": "sha256:abc", "mirrors": ["https://x/y.fxplug"],
            "yanked": "none"
          }]
        }"#;
        let idx: MarketIndex = serde_json::from_str(json).expect("parse");
        assert_eq!(idx.index_id, "00000000-0000-0000-0000-000000000000");
        assert_eq!(idx.sequence, 3);
        assert_eq!(idx.entries.len(), 1);
        assert_eq!(idx.entries[0].plugin_id, "a@b");
        assert_eq!(idx.entries[0].sig_scheme, "none"); // 默认值
    }

    fn claim_entry(permissions: &[&str]) -> super::MarketEntry {
        let json = serde_json::json!({
            "pluginId": "zerx@demo",
            "version": "1.0.0",
            "sequence": 1,
            "contentHash": "sha256:00",
            "permissions": permissions,
        });
        serde_json::from_value(json).expect("entry")
    }

    fn fxplug(identity: &str, version: &str, permissions: &[&str]) -> Vec<u8> {
        use std::io::Write;
        let manifest = serde_json::json!({
            "identity": identity,
            "name": "Demo",
            "version": version,
            "permissions": permissions,
        });
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("manifest.json", options).expect("start");
        writer
            .write_all(manifest.to_string().as_bytes())
            .expect("write");
        writer.finish().expect("finish").into_inner()
    }

    struct TempRoot(std::path::PathBuf);
    impl TempRoot {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("fx_market_claim_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("mkdir");
            Self(dir)
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("cleanup temp root");
        }
    }

    fn install_claimed(
        root: &std::path::Path,
        entry: &super::MarketEntry,
        bytes: &[u8],
    ) -> Result<(), super::PluginError> {
        let outcome = crate::plugin::install::install_from_zip_with_backup(root, bytes, |m| {
            super::verify_package_claim(entry, m)
        })?;
        crate::plugin::install::commit_install(&outcome)
    }

    fn dir_names(root: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn package_claim_accepts_equal_and_subset_permissions() {
        let root = TempRoot::new();
        let entry = claim_entry(&["ffmpeg", "auth"]);
        install_claimed(
            &root.0,
            &entry,
            &fxplug("zerx@demo", "1.0.0", &["ffmpeg", "auth"]),
        )
        .expect("equal");
        install_claimed(&root.0, &entry, &fxplug("zerx@demo", "1.0.0", &["ffmpeg"]))
            .expect("subset");
        install_claimed(&root.0, &entry, &fxplug("zerx@demo", "1.0.0", &[])).expect("empty");
        assert_eq!(dir_names(&root.0), vec!["zerx@demo".to_string()]);
    }

    #[test]
    fn package_claim_rejects_mismatch_and_keeps_installed_plugin() {
        let root = TempRoot::new();
        let entry = claim_entry(&["ffmpeg"]);
        install_claimed(&root.0, &entry, &fxplug("zerx@demo", "1.0.0", &["ffmpeg"])).expect("seed");
        let manifest_path = root.0.join("zerx@demo").join("manifest.json");
        let before = std::fs::read(&manifest_path).expect("read manifest");

        let cases = [
            (fxplug("zerx@other", "1.0.0", &["ffmpeg"]), "identity"),
            (fxplug("zerx@demo", "1.0.1", &["ffmpeg"]), "version"),
            (fxplug("zerx@demo", "1.0.0", &["ffmpeg", "auth"]), "auth"),
        ];
        for (bytes, needle) in cases {
            let err = install_claimed(&root.0, &entry, &bytes).expect_err("must reject");
            let super::PluginError::ManifestInvalid(msg) = err else {
                panic!("unexpected error variant: {err:?}");
            };
            assert!(msg.contains(needle), "{msg}");
            assert_eq!(
                std::fs::read(&manifest_path).expect("read manifest"),
                before
            );
            assert_eq!(dir_names(&root.0), vec!["zerx@demo".to_string()]);
        }
    }
}
