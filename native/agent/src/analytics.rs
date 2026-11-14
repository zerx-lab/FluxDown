//! 仅发送安装一次与每日活跃两类匿名部署事件。

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::http_client::LazyHttpClient;
use crate::state::{AgentState, StateStore};

const BAKED_APP_KEY: &str = match option_env!("FLUXDOWN_ANALYTICS_APP_KEY") {
    Some(value) => value,
    None => "",
};
const DEFAULT_ENDPOINT: &str =
    "https://ops.fluxdown.com/api/zerx.v1.AnalyticsIngestService/TrackEvents";

pub struct AnalyticsWorker {
    state: Arc<Mutex<AgentState>>,
    store: Arc<StateStore>,
    client: LazyHttpClient,
    endpoint: String,
    app_key: String,
}

impl AnalyticsWorker {
    #[must_use]
    pub fn new(state: Arc<Mutex<AgentState>>, store: Arc<StateStore>) -> Self {
        let endpoint = std::env::var("FLUXDOWN_ANALYTICS_ENDPOINT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());
        let app_key = std::env::var("FLUXDOWN_ANALYTICS_APP_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| BAKED_APP_KEY.to_owned());
        Self {
            state,
            store,
            client: LazyHttpClient::new(|| {
                crate::tls::client_builder(crate::tls::HttpVersion::Any)
                    .timeout(Duration::from_secs(15))
            }),
            endpoint,
            app_key,
        }
    }

    pub async fn run(self, cancel: CancellationToken) {
        if analytics_disabled_by_env() || self.app_key.trim().is_empty() {
            return;
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(10)) => {},
        }
        loop {
            self.report_once().await;
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(3600)) => {},
            }
        }
    }

    async fn report_once(&self) {
        let (enabled, analytics_id, installed, last_day) = {
            let mut state = self.state.lock().await;
            let enabled = state
                .preferences
                .values
                .get("analytics_enabled")
                .or_else(|| state.preferences.values.get("general.analytics_enabled"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            if enabled && state.analytics_id.is_empty() {
                state.analytics_id = uuid::Uuid::new_v4().to_string();
                if let Err(error) = self.store.save(&state).await {
                    tracing::warn!(error = %error, "persisting analytics id failed");
                }
            }
            (
                enabled,
                state.analytics_id.clone(),
                state.analytics_install_reported,
                state.analytics_last_active_day,
            )
        };
        if !enabled || analytics_id.is_empty() {
            return;
        }
        let device_id = analytics_id;
        let mut install_reported = installed;
        if !installed && self.track("app_installed", &device_id).await {
            install_reported = true;
        }
        let today = epoch_days();
        let active_reported = last_day == today || self.track("app_active", &device_id).await;
        if install_reported != installed || (active_reported && last_day != today) {
            let mut state = self.state.lock().await;
            state.analytics_install_reported = install_reported;
            if active_reported {
                state.analytics_last_active_day = today;
            }
            if let Err(error) = self.store.save(&state).await {
                tracing::warn!(error = %error, "persisting analytics markers failed");
            }
        }
    }

    async fn track(&self, event_name: &str, device_id: &str) -> bool {
        let payload = serde_json::json!({
            "events": [{
                "sessionId": device_id,
                "eventName": event_name,
                "systemProps": {
                    "osName": os_name(),
                    "osVersion": std::env::consts::ARCH,
                    "appVersion": fluxdown_protocol::APP_VERSION,
                    "locale": "",
                    "isDebug": cfg!(debug_assertions),
                },
                "props": {"edition": "desktop-agent"},
            }]
        });
        let client = match self.client.get().await {
            Ok(client) => client,
            Err(error) => {
                tracing::debug!(error = %error, event_name, "analytics client unavailable");
                return false;
            }
        };
        match client
            .post(&self.endpoint)
            .header("App-Key", &self.app_key)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => true,
            Ok(response) => {
                tracing::debug!(status = %response.status(), event_name, "analytics event rejected");
                false
            }
            Err(error) => {
                tracing::debug!(error = %error, event_name, "analytics event failed");
                false
            }
        }
    }
}

fn analytics_disabled_by_env() -> bool {
    matches!(
        std::env::var("FLUXDOWN_ANALYTICS").as_deref(),
        Ok("off") | Ok("0") | Ok("false")
    )
}

fn epoch_days() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 86_400)
        .unwrap_or(0)
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::epoch_days;

    #[test]
    fn epoch_day_is_stable_within_process() {
        assert_eq!(epoch_days(), epoch_days());
    }
}
