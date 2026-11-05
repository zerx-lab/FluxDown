//! FluxCloud `/api/v1` 资源调用面。

use reqwest::Method;
use serde::Serialize;
use serde_json::Value;

use super::{CloudClient, CloudError};

#[derive(Clone)]
pub struct CloudApi {
    client: CloudClient,
    epoch: Option<super::client::RequestEpoch>,
}

impl CloudApi {
    pub(crate) fn at_epoch(&self, epoch: super::client::RequestEpoch) -> Self {
        Self {
            client: self.client.clone(),
            epoch: Some(epoch),
        }
    }

    pub(crate) fn request_epoch(&self) -> super::client::RequestEpoch {
        self.epoch.unwrap_or_else(|| self.client.request_epoch())
    }

    pub(crate) async fn lock_epoch(
        &self,
        epoch: super::client::RequestEpoch,
    ) -> Result<tokio::sync::MutexGuard<'_, crate::state::AgentState>, CloudError> {
        self.client.lock_epoch(epoch).await
    }

    #[must_use]
    pub fn new(client: CloudClient) -> Self {
        Self {
            client,
            epoch: None,
        }
    }

    pub async fn profile(&self) -> Result<Value, CloudError> {
        self.authed(Method::GET, "/api/v1/me", None::<&Value>).await
    }

    pub async fn devices(&self, device_id: &str) -> Result<Value, CloudError> {
        self.authed(
            Method::GET,
            &format!("/api/v1/devices?deviceId={}", encode(device_id)),
            None::<&Value>,
        )
        .await
    }

    /// `PATCH /devices/current`：按令牌里的设备定位本机，上报默认目录 / 路径风格 / 版本。
    /// 旧版云端没有该端点（404）——调用方应忽略。
    pub async fn patch_current_device<P: Serialize>(&self, body: &P) -> Result<Value, CloudError> {
        self.authed(Method::PATCH, "/api/v1/devices/current", Some(body))
            .await
    }

    pub async fn rename_device(&self, id: &str, name: &str) -> Result<Value, CloudError> {
        self.authed(
            Method::PATCH,
            &format!("/api/v1/devices/{}", encode(id)),
            Some(&serde_json::json!({"name": name})),
        )
        .await
    }

    pub async fn delete_device(&self, id: &str) -> Result<Value, CloudError> {
        self.authed(
            Method::DELETE,
            &format!("/api/v1/devices/{}", encode(id)),
            None::<&Value>,
        )
        .await
    }

    /// `POST /devices/{id}/disconnect`：让同账号的另一台在线设备下线、释放在线名额（不删除设备）。
    pub async fn disconnect_device(&self, id: &str) -> Result<Value, CloudError> {
        self.authed(
            Method::POST,
            &format!("/api/v1/devices/{}/disconnect", encode(id)),
            None::<&Value>,
        )
        .await
    }

    /// 按 FluxCloud 时钟估计的当前 unix 毫秒；供配置同步的编辑时间戳使用。
    #[must_use]
    pub fn server_now_ms(&self) -> i64 {
        self.client.server_now_ms()
    }

    pub async fn remote_tasks(&self) -> Result<Value, CloudError> {
        self.authed(Method::GET, "/api/v1/tasks/remote", None::<&Value>)
            .await
    }

    pub async fn dispatch_remote<P: Serialize>(&self, body: &P) -> Result<Value, CloudError> {
        self.authed(Method::POST, "/api/v1/tasks/dispatch", Some(body))
            .await
    }

    pub async fn command_remote<P: Serialize>(
        &self,
        id: &str,
        body: &P,
    ) -> Result<Value, CloudError> {
        self.authed(
            Method::POST,
            &format!("/api/v1/tasks/{}/command", encode(id)),
            Some(body),
        )
        .await
    }

    pub async fn report_remote_status<P: Serialize>(
        &self,
        id: &str,
        body: &P,
    ) -> Result<Value, CloudError> {
        self.authed(
            Method::POST,
            &format!("/api/v1/tasks/{}/status", encode(id)),
            Some(body),
        )
        .await
    }

    pub async fn report_remote_progress<P: Serialize>(
        &self,
        body: &P,
    ) -> Result<Value, CloudError> {
        self.authed(Method::POST, "/api/v1/tasks/progress", Some(body))
            .await
    }

    pub async fn ping_presence(&self) -> Result<Value, CloudError> {
        self.authed(Method::POST, "/api/v1/tasks/presence", None::<&Value>)
            .await
    }

    pub async fn remote_events(&self, device_id: &str) -> Result<reqwest::Response, CloudError> {
        self.client
            .authenticated_stream_epoch(
                &format!("/api/v1/tasks/events?deviceId={}", encode(device_id)),
                self.request_epoch(),
            )
            .await
    }

    /// 任务实时通道（WebSocket）：心跳在连接内收发，取代 `remote_events` + `ping_presence`。
    pub(crate) async fn remote_socket(
        &self,
        device_id: &str,
    ) -> Result<super::CloudSocket, CloudError> {
        self.client
            .authenticated_socket_epoch(
                &format!("/api/v1/tasks/ws?deviceId={}", encode(device_id)),
                self.request_epoch(),
            )
            .await
    }

    pub async fn plans(&self) -> Result<Value, CloudError> {
        self.client
            .public::<Value, Value>(Method::GET, "/api/v1/plans/catalog", None)
            .await
    }

    /// 插件市场收藏概览：公开计数始终拉取；已登录时再拉本人收藏。
    /// 已登录判定读本机凭据；本人收藏请求失败（含 401）按错误返回，不静默降级。
    pub async fn plugin_favorites(
        &self,
    ) -> Result<fluxdown_protocol::PluginFavoritesDto, CloudError> {
        let counts: FavoriteCountsBody = self
            .client
            .public::<Value, _>(Method::GET, "/api/v1/plugins/favorites/counts", None)
            .await?;
        let signed_in = self.client.current_user_id().await.is_some();
        let mine = if signed_in {
            let body: FavoriteMineBody = self
                .client
                .authenticated_epoch::<Value, _>(
                    Method::GET,
                    "/api/v1/plugins/favorites",
                    None,
                    self.request_epoch(),
                )
                .await?;
            body.plugin_ids
        } else {
            Vec::new()
        };
        Ok(fluxdown_protocol::PluginFavoritesDto {
            counts: counts.counts,
            mine,
            signed_in,
        })
    }

    /// 收藏 / 取消收藏单个插件（需登录）。
    pub async fn set_plugin_favorite(
        &self,
        request: fluxdown_protocol::PluginFavoriteSetRequest,
    ) -> Result<fluxdown_protocol::PluginFavoriteResultDto, CloudError> {
        let method = if request.favorited {
            Method::PUT
        } else {
            Method::DELETE
        };
        self.client
            .authenticated_epoch::<Value, _>(
                method,
                &format!("/api/v1/plugins/favorites/{}", encode(&request.plugin_id)),
                None,
                self.request_epoch(),
            )
            .await
    }

    /// `GET /private-plugins`（账号鉴权）：当前账号被授权的私有插件目录与撤回清单。
    pub(crate) async fn private_plugins(&self) -> Result<PrivateCatalogBody, CloudError> {
        self.client
            .authenticated_epoch::<Value, _>(
                Method::GET,
                "/api/v1/private-plugins",
                None,
                self.request_epoch(),
            )
            .await
    }

    /// `POST /private-plugins/{pluginId}/download`（账号鉴权）：为某个授权版本换一张单次下载票据。
    /// 无授权 / 版本不存在统一 404。
    pub(crate) async fn private_plugin_ticket(
        &self,
        plugin_id: &str,
        version: &str,
    ) -> Result<PrivateDownloadTicket, CloudError> {
        self.client
            .authenticated_epoch(
                Method::POST,
                &format!("/api/v1/private-plugins/{}/download", encode(plugin_id)),
                Some(&serde_json::json!({ "version": version })),
                self.request_epoch(),
            )
            .await
    }

    pub async fn create_order<P: Serialize>(&self, body: &P) -> Result<Value, CloudError> {
        self.authed(Method::POST, "/api/v1/orders", Some(body))
            .await
    }

    pub async fn order(&self, order_no: &str) -> Result<Value, CloudError> {
        self.authed(
            Method::GET,
            &format!("/api/v1/orders/{}", encode(order_no)),
            None::<&Value>,
        )
        .await
    }

    pub async fn orders(&self) -> Result<Value, CloudError> {
        self.authed(Method::GET, "/api/v1/orders", None::<&Value>)
            .await
    }

    pub async fn referral(
        &self,
        suffix: &str,
        method: Method,
        body: Option<&Value>,
    ) -> Result<Value, CloudError> {
        self.authed(method, &format!("/api/v1/referral{suffix}"), body)
            .await
    }

    pub async fn referral_codes(&self, page: u32, page_size: u32) -> Result<Value, CloudError> {
        self.referral(
            &format!("/codes?page={page}&pageSize={page_size}"),
            Method::GET,
            None,
        )
        .await
    }

    pub async fn delete_referral_code(&self, id: &str) -> Result<Value, CloudError> {
        self.referral(&format!("/codes/{}", encode(id)), Method::DELETE, None)
            .await
    }

    pub async fn referral_records(
        &self,
        page: u32,
        page_size: u32,
        search: Option<&str>,
    ) -> Result<Value, CloudError> {
        let mut suffix = format!("/records?page={page}&pageSize={page_size}");
        if let Some(search) = search.filter(|value| !value.trim().is_empty()) {
            suffix.push_str("&search=");
            suffix.push_str(&encode(search.trim()));
        }
        self.referral(&suffix, Method::GET, None).await
    }

    pub async fn validate_referral(
        &self,
        code: &str,
        plan_code: &str,
    ) -> Result<Value, CloudError> {
        self.referral(
            &format!(
                "/validate?code={}&planCode={}",
                encode(code),
                encode(plan_code)
            ),
            Method::GET,
            None,
        )
        .await
    }

    pub async fn is_authenticated(&self) -> bool {
        self.client.is_authenticated().await
    }

    pub(crate) fn session_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.client.session_changes()
    }

    pub async fn sync_events(&self, device_id: &str) -> Result<reqwest::Response, CloudError> {
        self.client
            .authenticated_stream_epoch(
                &format!("/api/v1/sync/events?deviceId={}", encode(device_id)),
                self.request_epoch(),
            )
            .await
    }

    pub async fn sync_pull(&self, since: u64, device_id: &str) -> Result<Value, CloudError> {
        self.authed(
            Method::GET,
            &format!(
                "/api/v1/sync/items?since={since}&deviceId={}",
                encode(device_id)
            ),
            None::<&Value>,
        )
        .await
    }

    pub async fn sync_push<P: Serialize>(&self, body: &P) -> Result<Value, CloudError> {
        self.authed(Method::PUT, "/api/v1/sync/items", Some(body))
            .await
    }

    pub async fn cdn_config(&self) -> Result<Value, CloudError> {
        self.authed(Method::GET, "/api/v1/cdn/config", None::<&Value>)
            .await
    }

    pub async fn cdn_report<P: Serialize>(&self, body: &P) -> Result<Value, CloudError> {
        self.authed(Method::POST, "/api/v1/cdn/report", Some(body))
            .await
    }

    /// `POST /bt-guard/evidence`（账号鉴权）：BT 防吸血证据上报。遥测红线例外，唯一调用方是
    /// `bt_guard::evidence`；单批 ≤ 200 条由调用方保证。
    pub(crate) async fn bt_guard_evidence<P: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        body: &P,
    ) -> Result<R, CloudError> {
        self.authed_as(Method::POST, "/api/v1/bt-guard/evidence", Some(body))
            .await
    }

    // ---- 云端推送通知（契约 §3，`/api/v1/notifications/*`）----

    /// `GET /notifications/catalog`：公开接口（匿名），返回管理员启用的渠道种类（固定顺序）。
    pub async fn notify_catalog(
        &self,
    ) -> Result<Vec<fluxdown_protocol::CloudNotifyKindDto>, CloudError> {
        #[derive(serde::Deserialize)]
        struct Catalog {
            #[serde(default)]
            kinds: Vec<fluxdown_protocol::CloudNotifyKindDto>,
        }
        let catalog: Catalog = self
            .client
            .public::<Value, Catalog>(Method::GET, "/api/v1/notifications/catalog", None)
            .await?;
        Ok(catalog.kinds)
    }

    /// `GET /notifications/overview`。
    pub async fn notify_overview(
        &self,
    ) -> Result<fluxdown_protocol::CloudNotifyOverviewDto, CloudError> {
        self.notify_call(Method::GET, "/overview", None::<&Value>)
            .await
    }

    /// `POST /notifications/channels`。
    pub async fn notify_create_channel(
        &self,
        params: &fluxdown_protocol::CloudNotifyChannelCreateParams,
    ) -> Result<fluxdown_protocol::CloudNotifyChannelDto, CloudError> {
        self.notify_call(Method::POST, "/channels", Some(params))
            .await
    }

    /// `PATCH /notifications/channels/{id}`：只提交显式给出的字段（`id` 在路径里）。
    pub async fn notify_update_channel(
        &self,
        params: &fluxdown_protocol::CloudNotifyChannelUpdateParams,
    ) -> Result<fluxdown_protocol::CloudNotifyChannelDto, CloudError> {
        let mut body = serde_json::Map::new();
        if let Some(name) = &params.name {
            body.insert("name".to_owned(), Value::from(name.as_str()));
        }
        if let Some(enabled) = params.enabled {
            body.insert("enabled".to_owned(), Value::from(enabled));
        }
        if let Some(events) = &params.events {
            body.insert("events".to_owned(), Value::from(events.clone()));
        }
        if let Some(addresses) = &params.addresses {
            body.insert("addresses".to_owned(), Value::from(addresses.clone()));
        }
        self.notify_call(
            Method::PATCH,
            &format!("/channels/{}", encode(&params.id)),
            Some(&Value::Object(body)),
        )
        .await
    }

    /// `DELETE /notifications/channels/{id}`。
    pub async fn notify_delete_channel(&self, id: &str) -> Result<(), CloudError> {
        let _: Value = self
            .notify_call(
                Method::DELETE,
                &format!("/channels/{}", encode(id)),
                None::<&Value>,
            )
            .await?;
        Ok(())
    }

    /// `POST /notifications/email/code`：向非账号邮箱发 6 位验证码。
    pub async fn notify_send_email_code(
        &self,
        address: &str,
    ) -> Result<fluxdown_protocol::CloudNotifyEmailCodeResult, CloudError> {
        self.notify_call(
            Method::POST,
            "/email/code",
            Some(&serde_json::json!({ "address": address })),
        )
        .await
    }

    /// `POST /notifications/email/verify`：校验验证码，把地址记为本账号已验证的通知邮箱。
    pub async fn notify_verify_email(&self, address: &str, code: &str) -> Result<(), CloudError> {
        let _: Value = self
            .notify_call(
                Method::POST,
                "/email/verify",
                Some(&serde_json::json!({ "address": address, "code": code })),
            )
            .await?;
        Ok(())
    }

    /// `POST /notifications/channels/{id}/test`（同步发送，不计额度）。
    pub async fn notify_test_channel(
        &self,
        id: &str,
    ) -> Result<fluxdown_protocol::CloudNotifyTestResult, CloudError> {
        self.notify_call(
            Method::POST,
            &format!("/channels/{}/test", encode(id)),
            None::<&Value>,
        )
        .await
    }

    /// `POST /notifications/telegram/bind`。
    pub async fn notify_telegram_bind(
        &self,
    ) -> Result<fluxdown_protocol::CloudNotifyTelegramBindDto, CloudError> {
        self.notify_call(Method::POST, "/telegram/bind", None::<&Value>)
            .await
    }

    /// `GET /notifications/telegram/bind/{code}`。
    pub async fn notify_telegram_bind_status(
        &self,
        code: &str,
    ) -> Result<fluxdown_protocol::CloudNotifyTelegramBindStatusDto, CloudError> {
        self.notify_call(
            Method::GET,
            &format!("/telegram/bind/{}", encode(code)),
            None::<&Value>,
        )
        .await
    }

    /// `GET /notifications/deliveries?limit=&before=`。
    pub async fn notify_deliveries(
        &self,
        params: &fluxdown_protocol::CloudNotifyDeliveriesParams,
    ) -> Result<fluxdown_protocol::CloudNotifyDeliveriesPage, CloudError> {
        let mut query = format!("?limit={}", params.limit.unwrap_or(50).clamp(1, 100));
        if let Some(before) = params.before.as_deref().filter(|value| !value.is_empty()) {
            query.push_str("&before=");
            query.push_str(&encode(before));
        }
        self.notify_call(Method::GET, &format!("/deliveries{query}"), None::<&Value>)
            .await
    }

    /// `POST /notifications/events`：批量上报任务事件（≤ 50 条，由调用方分批）。
    pub async fn notify_report_events<P: Serialize>(
        &self,
        events: &[P],
    ) -> Result<NotifyReportResponse, CloudError> {
        self.notify_call(
            Method::POST,
            "/events",
            Some(&serde_json::json!({ "events": events })),
        )
        .await
    }

    async fn notify_call<P: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        suffix: &str,
        body: Option<&P>,
    ) -> Result<R, CloudError> {
        let value = self
            .authed(method, &format!("/api/v1/notifications{suffix}"), body)
            .await?;
        serde_json::from_value(value)
            .map_err(|error| CloudError::invalid_response(error.to_string()))
    }

    pub async fn profile_call<P: Serialize>(
        &self,
        method: Method,
        suffix: &str,
        body: Option<&P>,
    ) -> Result<Value, CloudError> {
        self.authed(method, &format!("/api/v1/me{suffix}"), body)
            .await
    }

    pub(crate) async fn persist_profile(
        &self,
        epoch: super::client::RequestEpoch,
        value: Value,
    ) -> Result<fluxdown_protocol::AgentSessionDto, CloudError> {
        let profile = serde_json::from_value::<fluxdown_protocol::CloudProfile>(value)
            .map_err(|error| CloudError::invalid_response(error.to_string()))?;
        self.client.persist_profile(epoch, profile).await
    }

    pub async fn clear_session(&self) -> Result<(), CloudError> {
        self.client.clear_session().await
    }

    pub(crate) async fn clear_session_epoch(
        &self,
        epoch: super::client::RequestEpoch,
    ) -> Result<(), CloudError> {
        self.client.clear_session_epoch(epoch).await
    }

    /// 非用户主动结束会话：先发 `SessionRevoked(reason)` 再清会话。
    pub async fn revoke_session(
        &self,
        reason: fluxdown_protocol::ErrorReason,
    ) -> Result<(), CloudError> {
        self.client.revoke_session(reason).await
    }

    pub(crate) async fn revoke_session_epoch(
        &self,
        reason: fluxdown_protocol::ErrorReason,
        epoch: super::client::RequestEpoch,
    ) -> Result<(), CloudError> {
        self.client.revoke_session_epoch(reason, epoch).await
    }

    /// 当前登录账号 id；未登录为 `None`。
    pub async fn current_user_id(&self) -> Option<String> {
        self.client.current_user_id().await
    }

    #[must_use]
    pub fn endpoint(&self) -> fluxdown_protocol::CloudEndpointDto {
        self.client.endpoint()
    }

    pub async fn set_endpoint(
        &self,
        base_url: &str,
    ) -> Result<fluxdown_protocol::CloudEndpointDto, CloudError> {
        self.client.set_endpoint(base_url).await
    }

    async fn authed_as<P: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&P>,
    ) -> Result<R, CloudError> {
        self.client
            .authenticated_epoch(method, path, body, self.request_epoch())
            .await
    }

    async fn authed<P: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&P>,
    ) -> Result<Value, CloudError> {
        self.client
            .authenticated_epoch(method, path, body, self.request_epoch())
            .await
    }
}

/// `GET /private-plugins` 响应（`revision` 等未使用字段忽略）。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrivateCatalogBody {
    #[serde(default)]
    pub entries: Vec<fluxdown_protocol::MarketEntryDto>,
    #[serde(default)]
    pub revoked: Vec<PrivateRevokedBody>,
    /// 该账号存在授权行（含过期、软撤销）的全部 pluginId。字段缺失（旧版云端）= `None`，
    /// 与「存在但为空」的 `Some([])` 不同：`None` 时客户端不得解封任何撤回。
    #[serde(default)]
    pub scope: Option<Vec<String>>,
}

/// 撤回清单项：用户授权范围内被标记的版本。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrivateRevokedBody {
    pub plugin_id: String,
    pub version: String,
    #[serde(default)]
    pub yanked: String,
}

/// `POST /private-plugins/{pluginId}/download` 响应。票据本身只存在于 `url` 里，是单次凭据：
/// 不实现 `Debug`，不进入日志。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrivateDownloadTicket {
    /// 相对 API 根的路径：`/api/v1/private-plugins/packages/{ticket}`。
    pub url: String,
    pub content_hash: String,
    pub bytes: u64,
}

/// `GET /plugins/favorites/counts` 响应。
#[derive(serde::Deserialize)]
struct FavoriteCountsBody {
    #[serde(default)]
    counts: std::collections::BTreeMap<String, u64>,
}

/// `GET /plugins/favorites` 响应。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct FavoriteMineBody {
    #[serde(default)]
    plugin_ids: Vec<String>,
}

/// `POST /notifications/events` 响应：逐条结果与最新额度用量。
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyReportResponse {
    #[serde(default)]
    pub results: Vec<NotifyReportResult>,
    #[serde(default)]
    pub usage: Option<fluxdown_protocol::cloud_notify::CloudNotifyUsageDto>,
}

/// 单条事件的受理结果。
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyReportResult {
    pub delivery_id: String,
    /// `accepted` / `duplicate` / `no_channel` / `quota_exceeded` / `disabled`。
    pub outcome: String,
}

fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}
