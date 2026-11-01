//! FluxCloud HTTP 私有模型；含令牌的类型不得导出到本机协议。

use fluxdown_protocol::{
    AgentSessionDto, CloudDevice, CloudPlan, CloudSubscription, CloudUser, Entitlements,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub user: CloudUser,
    #[serde(default)]
    pub entitlements: Entitlements,
    pub current_plan: Option<CloudPlan>,
    #[serde(default)]
    pub active_subscription: Option<CloudSubscription>,
    pub device: CloudDevice,
}

impl AuthResponse {
    pub(crate) fn session(&self) -> AgentSessionDto {
        AgentSessionDto {
            user: self.user.clone(),
            entitlements: self.entitlements.clone(),
            current_plan: self.current_plan.clone(),
            active_subscription: self.active_subscription.clone(),
            device: self.device.clone(),
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct CloudErrorBody {
    pub code: Option<String>,
    pub message: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RefreshRequest<'a> {
    pub refresh_token: &'a str,
}
