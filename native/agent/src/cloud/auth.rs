//! FluxCloud 登录/验证/退出与无令牌本地会话投影。

use fluxdown_protocol::{AgentEvent, AgentLoginResult};
use reqwest::Method;
use serde::Serialize;
use serde_json::Value;

use super::client::{CloudClient, CloudError};
use super::models::AuthResponse;
use crate::event_hub::AgentEventHub;

#[derive(Clone)]
pub struct CloudAuthService {
    client: CloudClient,
    events: AgentEventHub,
}

impl CloudAuthService {
    #[must_use]
    pub fn new(client: CloudClient, events: AgentEventHub) -> Self {
        Self { client, events }
    }

    pub async fn login<P: Serialize>(&self, request: &P) -> Result<AgentLoginResult, CloudError> {
        self.authenticate("/api/v1/auth/login", request).await
    }

    pub async fn login_verify<P: Serialize>(
        &self,
        request: &P,
    ) -> Result<AgentLoginResult, CloudError> {
        self.authenticate("/api/v1/auth/login/verify", request)
            .await
    }

    pub async fn register<P: Serialize>(
        &self,
        request: &P,
    ) -> Result<AgentLoginResult, CloudError> {
        self.authenticate("/api/v1/auth/register", request).await
    }

    pub async fn register_verify<P: Serialize>(
        &self,
        request: &P,
    ) -> Result<AgentLoginResult, CloudError> {
        self.authenticate("/api/v1/auth/register/verify", request)
            .await
    }

    pub async fn send_code<P: Serialize>(&self, request: &P) -> Result<Value, CloudError> {
        self.client
            .public(Method::POST, "/api/v1/auth/code/send", Some(request))
            .await
    }

    /// 重置密码发码（未登录）。
    pub async fn send_password_reset_code<P: Serialize>(
        &self,
        request: &P,
    ) -> Result<Value, CloudError> {
        self.client
            .public(Method::POST, "/api/v1/auth/password/code", Some(request))
            .await
    }

    /// 重置密码（未登录）；云端 204 空正文，按 JSON null 解码后丢弃。不触碰本机会话。
    pub async fn reset_password<P: Serialize>(&self, request: &P) -> Result<(), CloudError> {
        self.client
            .public::<P, Value>(Method::POST, "/api/v1/auth/password/reset", Some(request))
            .await
            .map(|_| ())
    }

    pub async fn verify_code<P: Serialize>(
        &self,
        request: &P,
    ) -> Result<AgentLoginResult, CloudError> {
        self.authenticate("/api/v1/auth/code/verify", request).await
    }

    /// 退出：服务端吊销 + 本地清除由 [`CloudClient::logout`] 在刷新锁内完成。
    pub async fn logout(&self) -> Result<(), CloudError> {
        self.client.logout().await
    }

    /// FluxCloud 认证契约：认证类请求体必须携带
    /// `deviceId`（服务端必填，缺失 422）及 `deviceName` / `devicePlatform` / `appVersion`；
    /// 响应兼容两种形态——tagged（`/auth/login`：`{status, auth}` 或
    /// `{status: "deviceVerificationRequired", ttlSeconds, willReplaceDevices}`）
    /// 与裸 `AuthResponse`（`/auth/*/verify`）。
    async fn authenticate<P: Serialize>(
        &self,
        path: &str,
        request: &P,
    ) -> Result<AgentLoginResult, CloudError> {
        let mut body = serde_json::to_value(request)
            .map_err(|error| CloudError::invalid_response(error.to_string()))?;
        if let Value::Object(map) = &mut body {
            let (device_id, device_name, platform) = self.client.device_identity().await;
            map.insert("deviceId".to_owned(), Value::String(device_id));
            if !device_name.is_empty() {
                map.insert("deviceName".to_owned(), Value::String(device_name));
            }
            if !platform.is_empty() {
                map.insert("devicePlatform".to_owned(), Value::String(platform));
            }
            map.insert(
                "appVersion".to_owned(),
                Value::String(fluxdown_protocol::APP_VERSION.to_owned()),
            );
        }
        let value: Value = self.client.public(Method::POST, path, Some(&body)).await?;
        let auth_value = match value.get("auth") {
            Some(auth) if auth.get("accessToken").is_some() => Some(auth.clone()),
            _ if value.get("accessToken").is_some() => Some(value.clone()),
            _ => None,
        };
        if let Some(auth_value) = auth_value {
            let auth = serde_json::from_value::<AuthResponse>(auth_value)
                .map_err(|error| CloudError::invalid_response(error.to_string()))?;
            let session = self.client.persist_auth(auth).await?;
            self.events
                .publish(AgentEvent::SessionChanged(Box::new(Some(session.clone()))));
            Ok(AgentLoginResult::Ok {
                session: Box::new(session),
            })
        } else {
            Ok(AgentLoginResult::DeviceVerificationRequired {
                ttl_seconds: value.get("ttlSeconds").and_then(Value::as_u64).unwrap_or(0),
                will_replace_devices: value
                    .get("willReplaceDevices")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        }
    }
}

impl CloudError {
    pub(crate) fn invalid_response(message: String) -> Self {
        Self {
            status: None,
            code: Some("invalidResponse".to_owned()),
            message,
            retryable: false,
            unreachable: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use fluxdown_protocol::{AgentLoginResult, AgentSnapshot};
    use serde_json::{Value, json};
    use tokio::sync::Mutex;

    use super::CloudAuthService;
    use crate::cloud::CloudClient;
    use crate::event_hub::AgentEventHub;
    use crate::state::{AgentState, StateStore};

    /// 模拟 FluxCloud `/auth/login`：`deviceId` 必填（缺失 422），响应为 tagged 形态。
    async fn login(axum::Json(body): axum::Json<Value>) -> Response {
        if body.get("deviceId").and_then(Value::as_str) != Some("device1") {
            return StatusCode::UNPROCESSABLE_ENTITY.into_response();
        }
        axum::Json(json!({
            "status": "ok",
            "auth": {
                "accessToken": "access",
                "refreshToken": "refresh",
                "expiresIn": 3600,
                "user": {
                    "id": "u1",
                    "email": "user@example.com",
                    "nickname": "User",
                    "plan": "free",
                    "status": "active",
                    "createdAt": ""
                },
                "entitlements": {},
                "device": {
                    "id": "row1",
                    "deviceId": "device1",
                    "name": "Desktop",
                    "createdAt": "",
                    "lastSeenAt": ""
                }
            }
        }))
        .into_response()
    }

    #[tokio::test]
    async fn login_sends_device_identity_and_unwraps_tagged_auth() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind login mock");
        let address = listener.local_addr().expect("login mock address");
        let app = Router::new().route("/api/v1/auth/login", post(login));
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve login mock");
        });

        let dir = std::env::temp_dir().join(format!(
            "fluxdown_cloud_login_test_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let store = Arc::new(StateStore::open(dir.clone()).await.expect("state store"));
        let state = Arc::new(Mutex::new(AgentState {
            device_id: "device1".to_owned(),
            device_name: "Desktop".to_owned(),
            platform: "macos".to_owned(),
            ..AgentState::default()
        }));
        let client = CloudClient::new(format!("http://{address}"), state.clone(), store.clone())
            .expect("cloud client");
        let auth = CloudAuthService::new(client, AgentEventHub::new(AgentSnapshot::default()));

        let result = auth
            .login(&json!({ "account": "user@example.com", "password": "secret" }))
            .await
            .expect("login accepted");
        let AgentLoginResult::Ok { session } = result else {
            panic!("expected direct login");
        };
        assert_eq!(session.user.email, "user@example.com");
        let credentials = state.lock().await.credentials.clone().expect("credentials");
        assert_eq!(credentials.access_token, "access");

        drop(auth);
        drop(state);
        drop(store);
        if let Err(error) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(path = %dir.display(), error = %error, "remove login test directory");
        }
    }
}
