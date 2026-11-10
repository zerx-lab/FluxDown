//! FluxCloud 私有插件商店（`agent.privatePlugin.*`）。
//!
//! 目录与下载票据走 FluxCloud 账号通道（Bearer、401 单飞刷新）；包体经本机统一出口
//! （[`crate::egress`]，与应用更新同一套代理）按票据拉取，边读边计数并增量哈希：超过票据声明的字节数或 10 MiB
//! 立即中止，sha256 与目录条目的 `contentHash` 比对通过后才上传 daemon blob，由
//! `daemon.plugin.installPrivate` 再次核对 identity / version / 哈希 / 权限声明。
//!
//! 票据是单次凭据：只出现在 [`PrivateDownloadTicket::url`]，不写日志（连接错误一律 `without_url`），
//! 不交给统一出口里的插件改写（`verified_payload = false`），也不重试同一张票据。

use std::sync::Arc;
use std::time::Duration;

use fluxdown_protocol::{
    ApplicationErrorCode, ErrorReason, InstalledPlugin, MarketEntryDto, PluginInstallPrivateParams,
    PluginRevokePrivateParams, PrivatePluginInstallRequest, PrivatePluginListDto,
    PrivateRevocationDto, RpcErrorData, method,
};
use sha2::{Digest, Sha256};

use crate::capture::{BlobKind, DaemonBlobClient};
use crate::cloud::{CloudApi, CloudError};
use crate::daemon_client::DaemonClient;
use crate::egress::{Egress, EgressClient};

/// 私有包体积上限（与公开市场 / FluxCloud 上传上限一致）。
const MAX_PACKAGE_BYTES: u64 = 10 * 1024 * 1024;
/// 整次拉取的总超时：票据在 GET 开始时即被烧掉，120 秒 TTL 只约束「开始」，不约束传输时长。
const PACKAGE_TIMEOUT: Duration = Duration::from_secs(300);
/// 读空闲超时：连接挂死时尽早失败，不必等满总超时。
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// 下载票据路径前缀；服务端返回的 `url` 必须落在其下，防止票据被引到别的路径 / 主机。
const PACKAGE_PATH_PREFIX: &str = "/api/v1/private-plugins/packages/";
/// 私有目录条目的渠道标记。
const CHANNEL_PRIVATE: &str = "private";
/// 只有被标记为恶意的版本才强制禁用（设计稿 §4.3）。
const REVOKED_MALICIOUS: &str = "malicious";

/// daemon 侧动作（真实实现经 JSON-RPC 与 blob 通道；测试用假实现）。
#[async_trait::async_trait]
pub(crate) trait PrivatePluginHost: Send + Sync {
    async fn install_package(
        &self,
        bytes: Vec<u8>,
        entry: MarketEntryDto,
    ) -> Result<InstalledPlugin, RpcErrorData>;

    async fn revoke(
        &self,
        revoked: Vec<PrivateRevocationDto>,
        scope: Option<Vec<String>>,
    ) -> Result<(), RpcErrorData>;
}

struct DaemonHost {
    daemon: Arc<DaemonClient>,
    blobs: Arc<DaemonBlobClient>,
}

#[async_trait::async_trait]
impl PrivatePluginHost for DaemonHost {
    async fn install_package(
        &self,
        bytes: Vec<u8>,
        entry: MarketEntryDto,
    ) -> Result<InstalledPlugin, RpcErrorData> {
        let blob_id = self
            .blobs
            .upload(BlobKind::Plugin, bytes)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "private plugin blob upload failed");
                match error {
                    crate::capture::BlobError::Status(413) => {
                        RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
                            .with_reason(ErrorReason::PluginPackageTooLarge)
                    }
                    crate::capture::BlobError::Status(401 | 403) => {
                        RpcErrorData::new(ApplicationErrorCode::Unauthorized, false)
                    }
                    crate::capture::BlobError::Url(_) | crate::capture::BlobError::Decode => {
                        RpcErrorData::new(ApplicationErrorCode::Internal, false)
                    }
                    crate::capture::BlobError::Http(_) | crate::capture::BlobError::Status(_) => {
                        RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
                    }
                }
            })?;
        self.daemon
            .call::<_, InstalledPlugin>(
                method::DAEMON_PLUGIN_INSTALL_PRIVATE,
                Some(PluginInstallPrivateParams { blob_id, entry }),
            )
            .await
    }

    async fn revoke(
        &self,
        revoked: Vec<PrivateRevocationDto>,
        scope: Option<Vec<String>>,
    ) -> Result<(), RpcErrorData> {
        self.daemon
            .call::<_, serde_json::Value>(
                method::DAEMON_PLUGIN_REVOKE_PRIVATE,
                Some(PluginRevokePrivateParams { revoked, scope }),
            )
            .await
            .map(|_| ())
    }
}

/// 私有商店服务；廉价共享，网关持有一份。
pub struct PrivatePluginService {
    cloud: Arc<CloudApi>,
    host: Arc<dyn PrivatePluginHost>,
    egress: Egress,
    http: EgressClient,
}

impl PrivatePluginService {
    #[must_use]
    pub fn new(
        cloud: Arc<CloudApi>,
        daemon: Arc<DaemonClient>,
        blobs: Arc<DaemonBlobClient>,
    ) -> Self {
        let egress = Egress::daemon(Arc::clone(&daemon));
        Self::with_parts(cloud, Arc::new(DaemonHost { daemon, blobs }), egress)
    }

    pub(crate) fn with_parts(
        cloud: Arc<CloudApi>,
        host: Arc<dyn PrivatePluginHost>,
        egress: Egress,
    ) -> Self {
        Self {
            cloud,
            host,
            egress,
            http: EgressClient::new(|| {
                reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(PACKAGE_TIMEOUT)
                    .read_timeout(READ_IDLE_TIMEOUT)
                    // 票据请求不跟随重定向：落点不在 API 根下就是异常。
                    .redirect(reqwest::redirect::Policy::none())
                    .user_agent(concat!("fluxdown-agent/", env!("CARGO_PKG_VERSION")))
            }),
        }
    }

    /// 取目录并把撤回清单（`yanked=malicious`）与授权范围 `scope` 交给 daemon。`background` = 在后台
    /// spawn 提交撤回（list 不被 daemon 往返阻塞）；否则等待完成（周期同步用）。撤回失败只记日志。
    ///
    /// `scope` 缺失（旧版云端）时原样传 `None`，daemon 据此保持只增不清；`scope` 存在时即便
    /// `revoked` 为空也要提交——那正是「运营已解除全部撤回」的信号。scope 与 revoked 都为空则无事可做。
    async fn fetch_catalog(
        &self,
        background: bool,
    ) -> Result<crate::cloud::PrivateCatalogBody, RpcErrorData> {
        let mut catalog = self.cloud.private_plugins().await.map_err(cloud_error)?;
        let revoked: Vec<PrivateRevocationDto> = std::mem::take(&mut catalog.revoked)
            .into_iter()
            .filter(|item| item.yanked == REVOKED_MALICIOUS)
            .map(|item| PrivateRevocationDto {
                plugin_id: item.plugin_id,
                version: item.version,
            })
            .collect();
        let scope = catalog.scope.take();
        if !revoked.is_empty() || scope.as_ref().is_some_and(|scope| !scope.is_empty()) {
            let host = Arc::clone(&self.host);
            let apply = async move {
                if let Err(error) = host.revoke(revoked, scope).await {
                    tracing::warn!(code = ?error.code, "private plugin revocation was not applied");
                }
            };
            if background {
                tokio::spawn(apply);
            } else {
                apply.await;
            }
        }
        Ok(catalog)
    }

    /// 本机登录态是否存在（不发请求）。
    pub async fn is_signed_in(&self) -> bool {
        self.cloud.is_authenticated().await
    }

    /// 登录 / 登出变化通知（与云客户端同一 watch）。
    pub fn session_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.cloud.session_changes()
    }

    /// 当前登录用户 id（本机凭据；不发请求）；未登录 `None`。
    pub async fn current_user_id(&self) -> Option<String> {
        self.cloud.current_user_id().await
    }

    /// 周期 / 登录后的撤回同步：已登录才拉目录，把 malicious 撤回清单与授权范围交 daemon（同步等待）。
    /// 未登录不发请求。daemon 只在撤回 / 解封实际生效时才写盘。
    pub async fn sync_revocations(&self) -> Result<(), RpcErrorData> {
        if !self.is_signed_in().await {
            return Ok(());
        }
        match self.fetch_catalog(false).await {
            Ok(_) => Ok(()),
            // 旧版 FluxCloud 没有该路由：无事可做。
            Err(error) if error.reason == Some(ErrorReason::PluginNotInMarket) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// `agent.privatePlugin.list`。未登录不发任何请求；撤回清单在后台交给 daemon。
    /// 云端 404（旧版 FluxCloud 无此路由）按「已登录、无条目」处理。
    pub async fn list(&self) -> Result<PrivatePluginListDto, RpcErrorData> {
        if !self.is_signed_in().await {
            return Ok(PrivatePluginListDto {
                signed_in: false,
                entries: Vec::new(),
            });
        }
        let catalog = match self.fetch_catalog(true).await {
            Ok(catalog) => catalog,
            Err(error) if error.reason == Some(ErrorReason::PluginNotInMarket) => {
                return Ok(PrivatePluginListDto {
                    signed_in: true,
                    entries: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        Ok(PrivatePluginListDto {
            signed_in: true,
            entries: catalog.entries.into_iter().map(private_entry).collect(),
        })
    }

    /// `agent.privatePlugin.install`。
    pub async fn install(
        &self,
        request: PrivatePluginInstallRequest,
    ) -> Result<InstalledPlugin, RpcErrorData> {
        if !self.cloud.is_authenticated().await {
            return Err(CloudError::unauthorized().to_rpc_error());
        }
        // 以云端当前目录为准重取条目：授权界面展示的权限 / 主机必须与即将安装的版本一致。
        let catalog = self.cloud.private_plugins().await.map_err(cloud_error)?;
        let entries: Vec<MarketEntryDto> = catalog.entries.into_iter().map(private_entry).collect();
        let entry = installable(&entries, &request.plugin_id)
            .ok_or_else(not_in_store)?
            .clone();
        if entry.version != request.version {
            return Err(RpcErrorData::new(ApplicationErrorCode::Conflict, false)
                .with_reason(ErrorReason::MarketVersionChanged));
        }

        let ticket = self
            .cloud
            .private_plugin_ticket(&entry.plugin_id, &entry.version)
            .await
            .map_err(cloud_error)?;
        if !same_hash(&ticket.content_hash, &entry.content_hash) {
            tracing::warn!(
                plugin = %entry.plugin_id,
                version = %entry.version,
                "private plugin ticket hash differs from the catalog entry"
            );
            return Err(download_rejected());
        }
        if ticket.bytes > MAX_PACKAGE_BYTES {
            return Err(too_large());
        }
        if ticket.bytes == 0 || !ticket_url_ok(&ticket.url) {
            tracing::warn!(
                plugin = %entry.plugin_id,
                "private plugin ticket was rejected: unexpected shape"
            );
            return Err(download_rejected());
        }

        let package_url = format!(
            "{}{}",
            self.cloud.endpoint().base_url.trim_end_matches('/'),
            ticket.url
        );

        let (bytes, actual_hash) = self.fetch_package(&package_url, ticket.bytes).await?;
        if !same_hash(&actual_hash, &entry.content_hash) {
            tracing::warn!(
                plugin = %entry.plugin_id,
                version = %entry.version,
                "private plugin package hash mismatch"
            );
            return Err(download_rejected());
        }
        self.host.install_package(bytes, entry).await
    }

    /// 凭票据拉包：返回 `(字节, sha256 小写 hex)`。读到的字节数一旦超过 `declared` 立即中止。
    async fn fetch_package(
        &self,
        url: &str,
        declared: u64,
    ) -> Result<(Vec<u8>, String), RpcErrorData> {
        let plan = self.egress.plan(url, false).await;
        // 票据只能交给原地址：不使用统一出口里的插件改写。
        let Some(attempt) = plan
            .attempts()
            .iter()
            .find(|attempt| attempt.plugin.is_none())
        else {
            return Err(download_failed());
        };
        let client = self.http.get(&plan.network).await.map_err(|error| {
            tracing::warn!(%error, "private plugin HTTP client could not be built");
            RpcErrorData::new(ApplicationErrorCode::Internal, false)
        })?;
        let mut response = client
            .get(&attempt.url)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            tracing::warn!(
                status = status.as_u16(),
                "private plugin package request rejected"
            );
            return Err(download_failed());
        }
        if response
            .content_length()
            .is_some_and(|length| length > declared)
        {
            return Err(too_large());
        }
        let capacity = usize::try_from(declared).map_err(|_| too_large())?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut hasher = Sha256::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            let received = u64::try_from(bytes.len())
                .unwrap_or(u64::MAX)
                .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if received > declared {
                return Err(too_large());
            }
            hasher.update(&chunk);
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, hex::encode(hasher.finalize())))
    }
}

/// 目录条目归一：渠道恒为 `private`，永不转发镜像地址。
fn private_entry(mut entry: MarketEntryDto) -> MarketEntryDto {
    entry.channel = Some(CHANNEL_PRIVATE.to_owned());
    entry.mirrors.clear();
    entry
}

/// 某插件当前可装版本：`yanked == "none"` 中 `sequence` 最大者（与客户端目录选版规则一致）。
fn installable<'a>(entries: &'a [MarketEntryDto], plugin_id: &str) -> Option<&'a MarketEntryDto> {
    entries
        .iter()
        .filter(|entry| entry.plugin_id == plugin_id && entry.yanked == "none")
        .max_by_key(|entry| entry.sequence)
}

fn same_hash(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        value
            .strip_prefix("sha256:")
            .unwrap_or(value)
            .to_ascii_lowercase()
    };
    normalize(left) == normalize(right)
}

/// FluxCloud 的 404（无授权与不存在同形）统一为「不在商店」。
fn cloud_error(error: CloudError) -> RpcErrorData {
    if error.status == Some(404) {
        return not_in_store();
    }
    error.to_rpc_error()
}

fn not_in_store() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::NotFound, false)
        .with_reason(ErrorReason::PluginNotInMarket)
}

fn download_failed() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::Unavailable, true)
        .with_reason(ErrorReason::PluginDownloadFailed)
}

/// 确定性的校验失败（哈希不符 / 票据形状异常）：重试同一版本不会变好。
fn download_rejected() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::Unavailable, false)
        .with_reason(ErrorReason::PluginDownloadFailed)
}

/// 票据 `url` = 固定前缀 + 16–128 位 `[A-Za-z0-9_-]` 票据段（不含 `/ ? # .`）。
fn ticket_url_ok(url: &str) -> bool {
    url.strip_prefix(PACKAGE_PATH_PREFIX).is_some_and(|ticket| {
        (16..=128).contains(&ticket.len())
            && ticket
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn too_large() -> RpcErrorData {
    RpcErrorData::new(ApplicationErrorCode::InvalidArgument, false)
        .with_reason(ErrorReason::PluginPackageTooLarge)
}

/// 传输层失败：`reqwest` 的错误文本含完整 URL（即票据），先摘掉。
fn transport_error(error: reqwest::Error) -> RpcErrorData {
    tracing::warn!(error = %error.without_url(), "private plugin package transfer failed");
    download_failed()
}

#[cfg(test)]
mod tests;
