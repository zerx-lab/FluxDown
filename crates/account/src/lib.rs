//! GPUI 账户、认证、设备、订单、配置同步与推介 capability。
//!
//! 账号页面结构：
//! 顶部居中的卡片列——未登录 hero / 已登录 profile、账号与安全、设备、云功能。

mod assets;
mod billing;
mod controller;
mod device_list;
mod dialogs;
mod errors;
mod host;
mod link;
mod pages;
mod profile_edit;
mod rail;
mod referral;
mod rpc;
mod sync_scope;
mod ui;
mod verification;
mod view;

use std::future::Future;
use std::pin::Pin;

use fluxdown_ui_i18n::Translator;
use gpui::SharedString;

pub use assets::{AccountAssets, CLOUD_ICON_PATH, CROWN_ICON_PATH};
pub use controller::AccountController;
pub use dialogs::add_device::open as open_add_device;
pub use dialogs::device::{confirm_unpair_by_fingerprint, open_link_rename_by_fingerprint};
pub use dialogs::pairing_prompt::open as open_pairing_prompt;
pub use dialogs::plan_purchase::open as open_plan_purchase;
pub use host::{AccountHost, AccountHostEvent};
pub use rail::AccountRailButton;
pub use referral::ReferralView;
pub use view::AccountView;

pub type PortFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, fluxdown_protocol::RpcErrorData>> + Send + 'static>>;

pub enum AccountCommand {
    Auth {
        method: &'static str,
        params: serde_json::Value,
    },
    Profile {
        method: &'static str,
        params: serde_json::Value,
    },
    Device {
        method: &'static str,
        params: serde_json::Value,
    },
    Plan {
        method: &'static str,
        params: serde_json::Value,
    },
    Order {
        method: &'static str,
        params: serde_json::Value,
    },
    Referral {
        method: &'static str,
        params: serde_json::Value,
    },
    /// 配置同步开关与状态（`agent.sync.*`）。
    Sync {
        method: &'static str,
        params: serde_json::Value,
    },
    /// 局域网直连配对与已配对设备（`agent.link.*`）。
    Link {
        method: &'static str,
        params: serde_json::Value,
    },
    /// FluxCloud 服务地址读取/覆盖（`agent.cloud.endpoint*`，仅调试构建可改）。
    CloudEndpoint {
        method: &'static str,
        params: serde_json::Value,
    },
}

pub trait AccountPort: Send + Sync {
    fn execute(&self, command: AccountCommand) -> PortFuture<serde_json::Value>;
}

/// 查表并转为可克隆的 `SharedString`；本 crate 内部统一走这层小转换。
pub(crate) fn t(translator: &Translator, key: &str) -> SharedString {
    SharedString::from(translator.text(key).to_owned())
}

/// 查表并做 `{name}` 占位插值。
pub(crate) fn t_with(
    translator: &Translator,
    key: &str,
    arguments: &[(&str, &str)],
) -> SharedString {
    SharedString::from(translator.text_with(key, arguments))
}
