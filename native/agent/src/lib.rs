//! FluxDown 官方客户端本地代理的应用边界。
//!
//! 本 crate 承载账户、云同步、设备协同与 UI Gateway；下载执行和下载任务事实属于
//! `fluxdown_daemon`。

pub mod analytics;
pub mod api_host;
#[cfg(feature = "desktop")]
mod app_icon;
pub mod background_effects;
pub mod bt_guard;
mod bundle_watch;
pub mod capture;
mod category_dir;
pub mod cdn_worker;
#[cfg(feature = "desktop")]
pub mod clipboard_watch;
pub mod cloud;
pub mod cloud_notify;
pub mod daemon_client;
mod demo;
mod device_identity;
pub mod device_meta;
pub mod diagnostics;
pub mod egress;
pub mod embedded;
pub mod event_hub;
mod event_pump;
pub mod gateway;
#[cfg(feature = "desktop")]
mod hotkey;
mod http_client;
pub mod lifecycle;
pub mod link;
pub mod log_export;
pub mod logging;
pub mod nmh;
pub mod notification;
mod open_association;
pub mod permission;
pub mod platform;
pub mod plugin_revocation;
pub mod power;
pub mod remote;
pub mod runtime;
pub mod server_mode;
pub mod shell;
mod signed_envelope;
pub mod sniff_rules;
mod sound;
mod startup_failure;
pub mod state;
pub mod supervisor;
pub mod sync;
mod system_proxy;
mod task_events;
pub mod update;
mod web_assets;

pub use category_dir::route_save_dir;
pub use embedded::{AgentStartError, EmbeddedAgent, EmbeddedConfig, start_embedded};
pub use gateway::{LocalConnection, LocalEventError};

use fluxdown_protocol::{ServiceHello, ServiceRole};

/// 官方客户端本地代理进程名。
pub const SERVICE_NAME: &str = "fluxdown-agent";

/// 返回本地代理用于协议协商的稳定身份。
#[must_use]
pub fn service_hello(instance_id: impl Into<String>, capabilities: Vec<String>) -> ServiceHello {
    ServiceHello::new(
        ServiceRole::Agent,
        SERVICE_NAME,
        fluxdown_protocol::APP_VERSION,
        instance_id,
        capabilities,
    )
}

#[cfg(test)]
mod tests {
    use fluxdown_protocol::{PROTOCOL_VERSION, ServiceRole};

    use super::{SERVICE_NAME, service_hello};

    #[test]
    fn identifies_as_cloud_agent() {
        let hello = service_hello("agent-instance", Vec::new());

        assert_eq!(hello.role, ServiceRole::Agent);
        assert_eq!(hello.service_name, SERVICE_NAME);
        assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
        assert_eq!(hello.instance_id, "agent-instance");
    }
}
