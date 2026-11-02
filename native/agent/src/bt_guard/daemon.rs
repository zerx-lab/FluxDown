//! agent 调 daemon 的 BT 防吸血内部方法。
//!
//! `daemon.bt.guard.cloud.apply` 与 `daemon.bt.guard.evidence.drain` 只有本模块调用：UI Gateway
//! 与远端 `/rpc` 一律拒绝这两个方法（见 `gateway::internal_daemon_method`）。用 trait 隔开
//! 真实 [`DaemonClient`]，让云名单与上报状态机可以在没有 daemon 的单测里确定性地驱动。

use async_trait::async_trait;
use fluxdown_protocol::{
    BtGuardCloudApplyDto, BtGuardCloudApplyParams, BtGuardEvidenceDrainDto, BtGuardEvidenceItemDto,
    BtGuardStateDto, method,
};
use serde_json::Value;

use crate::daemon_client::DaemonClient;

/// daemon 调用失败的可读原因（不含任何云名单 / 证据内容）。
pub(crate) type DaemonError = String;

#[async_trait]
pub(crate) trait GuardDaemon: Send + Sync {
    /// `daemon.bt.guard.cloud.apply`：交付已验签的云名单。拒绝（回滚 / 差异防护 / 过期……）不是
    /// 错误：`applied = false` 并带 `rejectedReason`。
    async fn apply_cloud(
        &self,
        params: &BtGuardCloudApplyParams,
    ) -> Result<BtGuardCloudApplyDto, DaemonError>;

    /// `daemon.bt.guard.evidence.drain`：弹出待上报证据。
    async fn drain_evidence(&self) -> Result<Vec<BtGuardEvidenceItemDto>, DaemonError>;

    /// `daemon.bt.guard.state`：BT 会话是否存活、当前云名单序号。
    async fn state(&self) -> Result<BtGuardStateDto, DaemonError>;
}

#[async_trait]
impl GuardDaemon for DaemonClient {
    async fn apply_cloud(
        &self,
        params: &BtGuardCloudApplyParams,
    ) -> Result<BtGuardCloudApplyDto, DaemonError> {
        self.call_detailed(method::DAEMON_BT_GUARD_CLOUD_APPLY, Some(params))
            .await
            .map_err(|error| error.message)
    }

    async fn drain_evidence(&self) -> Result<Vec<BtGuardEvidenceItemDto>, DaemonError> {
        let drained: BtGuardEvidenceDrainDto = self
            .call_detailed(method::DAEMON_BT_GUARD_EVIDENCE_DRAIN, None::<Value>)
            .await
            .map_err(|error| error.message)?;
        Ok(drained.items)
    }

    async fn state(&self) -> Result<BtGuardStateDto, DaemonError> {
        self.call_detailed(method::DAEMON_BT_GUARD_STATE, None::<Value>)
            .await
            .map_err(|error| error.message)
    }
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::method;
    use serde_json::json;

    use super::*;

    /// 真实 `DaemonClient` 发出的方法名与参数形状（daemon 侧按此解析）。
    #[tokio::test]
    async fn daemon_client_calls_the_contract_methods() {
        let (client, calls) = DaemonClient::recording();
        let params = BtGuardCloudApplyParams {
            envelope: "{\"payload\":\"e30=\"}".to_owned(),
            sequence: 7,
            verified_payload: json!({"sequence": 7}),
        };
        // 录制客户端对每个调用回 `{}`：结果按 DTO 缺省处理，这里只断言发出的调用。
        let applied = client.apply_cloud(&params).await.unwrap_or_default();
        let drained = client.drain_evidence().await.unwrap_or_default();
        let state = client.state().await.unwrap_or_default();
        assert!(!applied.applied && drained.is_empty() && !state.session_active);
        let calls = calls.lock().await;
        let methods: Vec<&str> = calls.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            methods,
            [
                method::DAEMON_BT_GUARD_CLOUD_APPLY,
                method::DAEMON_BT_GUARD_EVIDENCE_DRAIN,
                method::DAEMON_BT_GUARD_STATE,
            ]
        );
        assert_eq!(
            calls[0].1,
            Some(json!({
                "envelope": "{\"payload\":\"e30=\"}",
                "sequence": 7,
                "verifiedPayload": {"sequence": 7},
            }))
        );
        assert_eq!(calls[1].1, None);
    }

    #[tokio::test]
    async fn disconnected_daemon_surfaces_as_an_error_string() {
        let client = DaemonClient::disconnected();
        assert!(client.drain_evidence().await.is_err());
        assert!(client.state().await.is_err());
    }
}
